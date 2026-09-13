//! Pure HTML form submission primitives.
//!
//! Controls remain ordered pairs. A map would silently lose repeated names,
//! which are common for checkboxes and are observable by the server.

use alloc::string::String;

use crate::document::{ControlKind, Document, FormMethod};
use crate::limits::{MAX_ENCODED_REQUEST_BYTES, MAX_URL_BYTES};
use crate::memory;
use crate::request::Request;
use crate::url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    TooLong,
    OutOfMemory,
    NoSuchForm,
    UnsupportedMethod,
    /// A textarea's initial text did not fit its value limit, so what the
    /// form holds is not what the page sent.
    ValueOverflow,
}

/// Builds a GET target from one form and the page-owned current values.
/// `values` uses control IDs as indices; a missing entry falls back to the
/// HTML initial value. Only the activated submit control contributes.
pub fn submit_get(
    document: &Document,
    form_index: usize,
    values: &[String],
    checked: &[bool],
    selected: &[bool],
    activated_submit: Option<usize>,
) -> Result<Url, Error> {
    let form = document.forms().get(form_index).ok_or(Error::NoSuchForm)?;
    if form.method != FormMethod::Get {
        return Err(Error::UnsupportedMethod);
    }
    let fields = successful_controls(
        document,
        form_index,
        values,
        checked,
        selected,
        activated_submit,
    )?;
    get_url(&form.action, &fields)
}

/// Builds the complete GET or urlencoded POST request for one form.
/// `checked` holds the page-owned checkedness by control ID; a missing entry
/// falls back to the HTML initial checkedness. `selected` does the same for
/// select options by option ID.
pub fn submit(
    document: &Document,
    form_index: usize,
    values: &[String],
    checked: &[bool],
    selected: &[bool],
    activated_submit: Option<usize>,
) -> Result<Request, Error> {
    let form = document.forms().get(form_index).ok_or(Error::NoSuchForm)?;
    let fields = successful_controls(
        document,
        form_index,
        values,
        checked,
        selected,
        activated_submit,
    )?;
    match form.method {
        FormMethod::Get => Ok(Request::get(get_url(&form.action, &fields)?)),
        FormMethod::Post => {
            let body = encode(&fields, MAX_ENCODED_REQUEST_BYTES)?;
            let url = form
                .action
                .without_fragment()
                .map_err(|error| match error {
                    crate::url::Error::TooLong => Error::TooLong,
                    _ => Error::OutOfMemory,
                })?;
            Request::post_urlencoded(url, body).map_err(|error| match error {
                crate::request::Error::TooLong => Error::TooLong,
                crate::request::Error::InvalidHeadValue | crate::request::Error::OutOfMemory => {
                    Error::OutOfMemory
                }
            })
        }
        FormMethod::Unsupported => Err(Error::UnsupportedMethod),
    }
}

fn successful_controls<'a>(
    document: &'a Document,
    form_index: usize,
    values: &'a [String],
    checked: &[bool],
    selected: &[bool],
    activated_submit: Option<usize>,
) -> Result<alloc::vec::Vec<(&'a str, &'a str)>, Error> {
    let form = document.forms().get(form_index).ok_or(Error::NoSuchForm)?;
    let mut fields = alloc::vec::Vec::new();
    let start = form.first_control as usize;
    let end = start + form.control_count as usize;
    for (index, control) in document.controls()[start..end].iter().enumerate() {
        let control_id = start + index;
        if control.disabled || control.name.is_empty() {
            continue;
        }
        if control.value_overflow {
            return Err(Error::ValueOverflow);
        }
        let value = match control.kind {
            ControlKind::Text | ControlKind::Textarea => values
                .get(control_id)
                .map(String::as_str)
                .unwrap_or(&control.initial_value),
            ControlKind::Hidden => &control.initial_value,
            ControlKind::Submit if activated_submit == Some(control_id) => &control.initial_value,
            ControlKind::Submit => continue,
            // Unchecked checkboxes and radio buttons are not successful.
            ControlKind::Checkbox | ControlKind::Radio
                if checked.get(control_id).copied().unwrap_or(control.checked) =>
            {
                &control.initial_value
            }
            ControlKind::Checkbox | ControlKind::Radio => continue,
            // Every selected, enabled option is one pair, in option order.
            ControlKind::Select => {
                let first = control.first_option as usize;
                for (offset, option) in document.control_options(control).iter().enumerate() {
                    if option.disabled
                        || !selected
                            .get(first + offset)
                            .copied()
                            .unwrap_or(option.selected)
                    {
                        continue;
                    }
                    memory::push(&mut fields, (control.name.as_str(), option.value.as_str()))
                        .map_err(|_| Error::OutOfMemory)?;
                }
                continue;
            }
            // Non-submit button states are not successful controls.
            ControlKind::Unsupported => continue,
        };
        memory::push(&mut fields, (control.name.as_str(), value))
            .map_err(|_| Error::OutOfMemory)?;
    }
    Ok(fields)
}

/// Activates a checkbox or radio button in the page-owned `checked` state.
///
/// A checkbox toggles. A radio button becomes checked and clears the other
/// radio buttons of its group: the same form owner and the same non-empty
/// name. A nameless radio button is a group of its own. Activating a
/// checked radio button changes nothing. `changed` receives every control
/// whose checkedness changed, so the caller can repaint exactly those.
/// Returns false for anything that is not an enabled checkable control.
pub fn activate_checkable(
    document: &Document,
    checked: &mut [bool],
    control_id: usize,
    mut changed: impl FnMut(usize),
) -> bool {
    let controls = document.controls();
    let Some(control) = controls.get(control_id) else {
        return false;
    };
    if control.disabled || control_id >= checked.len() {
        return false;
    }
    match control.kind {
        ControlKind::Checkbox => {
            checked[control_id] = !checked[control_id];
            changed(control_id);
        }
        ControlKind::Radio => {
            if checked[control_id] {
                return true;
            }
            if !control.name.is_empty() {
                for (other_id, other) in controls.iter().enumerate() {
                    if other_id != control_id
                        && other.kind == ControlKind::Radio
                        && other.form == control.form
                        && other.name == control.name
                        && checked.get(other_id).copied() == Some(true)
                    {
                        checked[other_id] = false;
                        changed(other_id);
                    }
                }
            }
            checked[control_id] = true;
            changed(control_id);
        }
        _ => return false,
    }
    true
}

/// Chooses an option in the page-owned `selected` state, by option ID. In a
/// single select it becomes the only selected option; in a multiple select
/// it toggles. Returns false, changing nothing, for a disabled select or
/// option, or an option that is not this select's.
pub fn choose_option(
    document: &Document,
    selected: &mut [bool],
    control_id: usize,
    option_id: usize,
) -> bool {
    let Some(control) = document.controls().get(control_id) else {
        return false;
    };
    let first = control.first_option as usize;
    let end = first + control.option_count as usize;
    if control.kind != ControlKind::Select
        || control.disabled
        || !(first..end).contains(&option_id)
        || end > selected.len()
        || document.options()[option_id].disabled
    {
        return false;
    }
    if control.multiple {
        selected[option_id] = !selected[option_id];
    } else {
        for (id, slot) in selected[first..end].iter_mut().enumerate() {
            *slot = first + id == option_id;
        }
    }
    true
}

pub fn get_url(action: &Url, fields: &[(&str, &str)]) -> Result<Url, Error> {
    let query = encode_query(fields)?;
    action.with_query(query).map_err(|error| match error {
        crate::url::Error::TooLong => Error::TooLong,
        _ => Error::OutOfMemory,
    })
}

/// Builds the query part of `application/x-www-form-urlencoded` in UTF-8.
/// The returned text has no leading `?`.
pub fn encode_query(fields: &[(&str, &str)]) -> Result<String, Error> {
    encode(fields, MAX_URL_BYTES)
}

fn encode(fields: &[(&str, &str)], limit: usize) -> Result<String, Error> {
    let mut output = String::new();
    for (index, (name, value)) in fields.iter().enumerate() {
        if index != 0 {
            push_byte(&mut output, b'&', limit)?;
        }
        encode_component(&mut output, name, limit)?;
        push_byte(&mut output, b'=', limit)?;
        encode_component(&mut output, value, limit)?;
    }
    Ok(output)
}

fn encode_component(output: &mut String, value: &str, limit: usize) -> Result<(), Error> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        index += 1;
        match byte {
            // Form submission sends every line break as CRLF, whatever it
            // was stored as: a lone CR, a lone LF, or already CRLF.
            b'\r' | b'\n' => {
                if byte == b'\r' && bytes.get(index) == Some(&b'\n') {
                    index += 1;
                }
                for byte in *b"%0D%0A" {
                    push_byte(output, byte, limit)?;
                }
            }
            b' ' => push_byte(output, b'+', limit)?,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                push_byte(output, byte, limit)?;
            }
            _ => {
                push_byte(output, b'%', limit)?;
                push_byte(output, HEX[(byte >> 4) as usize], limit)?;
                push_byte(output, HEX[(byte & 0x0f) as usize], limit)?;
            }
        }
    }
    Ok(())
}

fn push_byte(output: &mut String, byte: u8, limit: usize) -> Result<(), Error> {
    if output.len() >= limit {
        return Err(Error::TooLong);
    }
    let encoded = [byte];
    // Every caller passes form syntax or uppercase hexadecimal: ASCII is
    // therefore valid UTF-8 and exactly one byte long.
    let text = unsafe { core::str::from_utf8_unchecked(&encoded) };
    memory::push_str(output, text).map_err(|_| Error::OutOfMemory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_duplicate_and_empty_fields_survive() {
        assert_eq!(
            encode_query(&[("q", "one"), ("q", "two"), ("empty", "")]).unwrap(),
            "q=one&q=two&empty="
        );
    }

    #[test]
    fn form_encoding_uses_utf8_plus_and_uppercase_percent_bytes() {
        assert_eq!(
            encode_query(&[("a b", "日本&=%")]).unwrap(),
            "a+b=%E6%97%A5%E6%9C%AC%26%3D%25"
        );
    }

    #[test]
    fn encoded_output_has_a_hard_bound() {
        let long = "%".repeat(MAX_URL_BYTES);
        assert_eq!(encode_query(&[("x", &long)]), Err(Error::TooLong));
    }

    #[test]
    fn get_replaces_the_actions_query_and_drops_its_fragment() {
        let action = Url::parse("http://example.com/find?old=1#result").unwrap();
        let result = get_url(&action, &[("q", "new value")]).unwrap();
        assert_eq!(
            result.to_text().unwrap(),
            "http://example.com/find?q=new+value"
        );
    }

    #[test]
    fn document_submission_filters_controls_without_losing_order() {
        let mut parser =
            crate::document::Parser::new(Url::parse("http://example.com/page").unwrap()).unwrap();
        parser
            .feed(b"<form action=/echo><input name=q value=old><input type=hidden name=q value=hidden><input name=skip disabled><input type=submit name=go value=Go></form>")
            .unwrap();
        let document = parser.finish().unwrap();
        let values = ["new value".into()];
        assert_eq!(
            submit_get(&document, 0, &values, &[], &[], Some(3))
                .unwrap()
                .to_text()
                .unwrap(),
            "http://example.com/echo?q=new+value&q=hidden&go=Go"
        );
    }

    #[test]
    fn unsupported_method_refuses_and_unimplemented_input_type_falls_back_to_text() {
        let mut parser =
            crate::document::Parser::new(Url::parse("http://example.com/page").unwrap()).unwrap();
        parser
            .feed(
                b"<form method=post><input name=q></form><form><input type=date name=when value=today><input type=submit></form>",
            )
            .unwrap();
        let document = parser.finish().unwrap();
        assert_eq!(
            submit_get(&document, 0, &[], &[], &[], None),
            Err(Error::UnsupportedMethod)
        );
        assert_eq!(
            submit_get(&document, 1, &[], &[], &[], Some(2))
                .unwrap()
                .to_text()
                .unwrap(),
            "http://example.com/page?when=today"
        );
    }

    #[test]
    fn post_keeps_the_action_query_and_encodes_the_body() {
        let mut parser =
            crate::document::Parser::new(Url::parse("http://example.com/page").unwrap()).unwrap();
        parser
            .feed(b"<form action='/echo?fixed=1#result' method=post><input name=q value=old><input type=hidden name=q value=hidden><button name=go value=Send>Send it</button></form>")
            .unwrap();
        let document = parser.finish().unwrap();
        let values = ["new value".into()];
        let request = submit(&document, 0, &values, &[], &[], Some(2)).unwrap();
        assert_eq!(request.method, crate::request::Method::Post);
        assert_eq!(
            request.url.to_text().unwrap(),
            "http://example.com/echo?fixed=1"
        );
        assert_eq!(request.body(), b"q=new+value&q=hidden&go=Send");
    }

    #[test]
    fn line_breaks_are_sent_as_crlf() {
        assert_eq!(
            encode_query(&[("t", "a\nb\r\nc\rd")]).unwrap(),
            "t=a%0D%0Ab%0D%0Ac%0D%0Ad"
        );
    }

    #[test]
    fn textarea_content_is_raw_text_and_its_value_is_submitted() {
        let mut parser =
            crate::document::Parser::new(Url::parse("http://example.com/page").unwrap()).unwrap();
        parser
            .feed(b"<p>before</p><form action=/echo><textarea name=t rows=30>\r\nfirst &amp; <b>line</b>\r\n</textare </textarea><textarea name=u rows=x></textarea></form><p>after</p>")
            .unwrap();
        let document = parser.finish().unwrap();
        let controls = document.controls();
        assert_eq!(controls[0].kind, ControlKind::Textarea);
        assert_eq!(controls[0].initial_value, "first & <b>line</b>\n</textare ");
        assert_eq!(controls[0].rows, crate::document::MAX_TEXTAREA_ROWS);
        assert_eq!(controls[1].rows, crate::document::DEFAULT_TEXTAREA_ROWS);
        assert!(!document.text().contains("line"));
        assert!(document.text().contains("after"));
        assert_eq!(
            submit_get(&document, 0, &[], &[], &[], None)
                .unwrap()
                .to_text()
                .unwrap(),
            "http://example.com/echo?t=first+%26+%3Cb%3Eline%3C%2Fb%3E%0D%0A%3C%2Ftextare+&u="
        );
        let values = ["one\ntwo".into(), String::new()];
        assert_eq!(
            submit_get(&document, 0, &values, &[], &[], None)
                .unwrap()
                .to_text()
                .unwrap(),
            "http://example.com/echo?t=one%0D%0Atwo&u="
        );
    }

    #[test]
    fn an_overflowing_textarea_refuses_to_submit() {
        let mut html = alloc::vec::Vec::new();
        html.extend_from_slice(b"<form><textarea name=t>");
        html.resize(html.len() + crate::limits::MAX_INPUT_VALUE_BYTES + 1, b'x');
        html.extend_from_slice(b"</textarea></form>");
        let mut parser =
            crate::document::Parser::new(Url::parse("http://example.com/page").unwrap()).unwrap();
        parser.feed(&html).unwrap();
        let document = parser.finish().unwrap();
        let control = &document.controls()[0];
        assert!(control.value_overflow);
        assert_eq!(
            control.initial_value.len(),
            crate::limits::MAX_INPUT_VALUE_BYTES
        );
        assert_eq!(
            submit_get(&document, 0, &[], &[], &[], None),
            Err(Error::ValueOverflow)
        );
    }

    fn select_document() -> Document {
        let mut parser =
            crate::document::Parser::new(Url::parse("http://example.com/page").unwrap()).unwrap();
        parser
            .feed(
                b"<p>before</p><form action=/echo>\
                  <select name=color><option value=red selected>Red</option>\
                  <option> Light   green </option>ignored<b>text</b>\
                  <option value=blue selected>Blue</select>\
                  <select name=none><option disabled>Off<option>On</option></select>\
                  <select name=tag multiple><optgroup label=g disabled>\
                  <option value=a selected>A</optgroup><option value=b selected>B\
                  <option>C</option></select>\
                  </form><p>after</p>",
            )
            .unwrap();
        parser.finish().unwrap()
    }

    #[test]
    fn select_parses_options_and_settles_initial_selection() {
        let document = select_document();
        let controls = document.controls();
        assert_eq!(controls.len(), 3);
        let labels: alloc::vec::Vec<(&str, &str, bool, bool)> = document
            .options()
            .iter()
            .map(|option| {
                (
                    option.label.as_str(),
                    option.value.as_str(),
                    option.selected,
                    option.disabled,
                )
            })
            .collect();
        assert_eq!(
            labels,
            [
                ("Red", "red", false, false),
                ("Light green", "Light green", false, false),
                ("Blue", "blue", true, false),
                ("Off", "Off", false, true),
                ("On", "On", true, false),
                ("A", "a", true, true),
                ("B", "b", true, false),
                ("C", "C", false, false),
            ]
        );
        assert!(controls[2].multiple);
        assert!(!document.text().contains("ignored"));
        assert!(document.text().contains("after"));
        assert_eq!(
            submit_get(&document, 0, &[], &[], &[], None)
                .unwrap()
                .to_text()
                .unwrap(),
            "http://example.com/echo?color=blue&none=On&tag=b"
        );
    }

    #[test]
    fn choosing_replaces_a_single_selection_and_toggles_a_multiple_one() {
        let document = select_document();
        let mut selected: alloc::vec::Vec<bool> = document
            .options()
            .iter()
            .map(|option| option.selected)
            .collect();
        assert!(choose_option(&document, &mut selected, 0, 1));
        assert_eq!(&selected[..3], [false, true, false]);
        assert!(!choose_option(&document, &mut selected, 1, 3));
        assert!(!choose_option(&document, &mut selected, 0, 4));
        assert!(!choose_option(&document, &mut selected, 2, 5));
        assert!(choose_option(&document, &mut selected, 2, 7));
        assert!(choose_option(&document, &mut selected, 2, 6));
        assert_eq!(&selected[5..], [true, false, true]);
        assert_eq!(
            submit_get(&document, 0, &[], &[], &selected, None)
                .unwrap()
                .to_text()
                .unwrap(),
            "http://example.com/echo?color=Light+green&none=On&tag=C"
        );
    }

    fn checkable_document() -> Document {
        let mut parser =
            crate::document::Parser::new(Url::parse("http://example.com/page").unwrap()).unwrap();
        parser
            .feed(
                b"<form action=/echo>\
                  <input type=checkbox name=t value=news checked>\
                  <input type=checkbox name=t value=sport>\
                  <input type=checkbox name=t value=off checked disabled>\
                  <input type=checkbox name=agree>\
                  <input type=radio name=size value=small checked>\
                  <input type=radio name=size value=large checked>\
                  <input type=radio name=lone value=a>\
                  </form><form><input type=radio name=size value=other checked></form>",
            )
            .unwrap();
        parser.finish().unwrap()
    }

    #[test]
    fn only_checked_enabled_checkables_are_sent_with_their_values() {
        let document = checkable_document();
        let controls = document.controls();
        assert_eq!(controls[3].initial_value, "on");
        // The later `checked` in the same group wins; another form's group
        // is independent.
        assert!(!controls[4].checked);
        assert!(controls[5].checked);
        assert!(controls[7].checked);
        assert_eq!(
            submit_get(&document, 0, &[], &[], &[], None)
                .unwrap()
                .to_text()
                .unwrap(),
            "http://example.com/echo?t=news&size=large"
        );
        let checked = [false, true, true, true, true, false, false, true];
        assert_eq!(
            submit_get(&document, 0, &[], &checked, &[], None)
                .unwrap()
                .to_text()
                .unwrap(),
            "http://example.com/echo?t=sport&agree=on&size=small"
        );
    }

    #[test]
    fn activation_toggles_checkboxes_and_moves_radio_selection_within_a_group() {
        let document = checkable_document();
        let mut checked: alloc::vec::Vec<bool> = document
            .controls()
            .iter()
            .map(|control| control.checked)
            .collect();
        let mut changed = alloc::vec::Vec::new();

        assert!(activate_checkable(&document, &mut checked, 1, |id| changed.push(id)));
        assert_eq!(changed, [1]);
        assert!(checked[1]);
        assert!(!activate_checkable(&document, &mut checked, 2, |_| panic!(
            "disabled"
        )));
        assert!(checked[2]);

        changed.clear();
        assert!(activate_checkable(&document, &mut checked, 4, |id| changed.push(id)));
        assert_eq!(changed, [5, 4]);
        assert!(checked[4] && !checked[5] && checked[7]);

        changed.clear();
        assert!(activate_checkable(&document, &mut checked, 4, |id| changed.push(id)));
        assert!(changed.is_empty());
        assert!(!activate_checkable(
            &document,
            &mut checked,
            0usize.wrapping_sub(1),
            |_| {}
        ));
    }
}
