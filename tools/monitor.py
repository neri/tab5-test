#!/usr/bin/env python3
"""Monitor Tab5 USB-Serial/JTAG output across USB re-enumerations."""

import argparse
import sys
import time

import serial


BAUDRATE = 115_200


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "device",
        nargs="?",
        default="/dev/ttyACM0",
        help="serial device to monitor (default: /dev/ttyACM0)",
    )
    return parser.parse_args()


def open_port(device: str) -> serial.Serial:
    port = serial.Serial()
    port.port = device
    port.baudrate = BAUDRATE
    port.timeout = 0.25
    # Do not alter Tab5's boot strap/reset signals when opening the port.
    port.dtr = False
    port.rts = False
    port.open()
    return port


def main() -> None:
    device = parse_args().device
    while True:
        try:
            with open_port(device) as port:
                print(f"connected: {device}", file=sys.stderr)
                while True:
                    data = port.read(port.in_waiting or 1)
                    if data:
                        print(data.decode("utf-8", errors="replace"), end="", flush=True)
        except (OSError, serial.SerialException) as error:
            print(f"waiting for {device}: {error}", file=sys.stderr)
            time.sleep(0.5)


if __name__ == "__main__":
    main()
