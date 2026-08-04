#!/usr/bin/env python3
"""Monitor Tab5 USB-Serial/JTAG output across USB re-enumerations."""

import sys
import time

import serial


DEVICE = "/dev/ttyACM0"
BAUDRATE = 115_200


def open_port() -> serial.Serial:
    port = serial.Serial()
    port.port = DEVICE
    port.baudrate = BAUDRATE
    port.timeout = 0.25
    # Do not alter Tab5's boot strap/reset signals when opening the port.
    port.dtr = False
    port.rts = False
    port.open()
    return port


while True:
    try:
        with open_port() as port:
            print(f"connected: {DEVICE}", file=sys.stderr)
            while True:
                data = port.read(port.in_waiting or 1)
                if data:
                    print(data.decode("utf-8", errors="replace"), end="", flush=True)
    except (OSError, serial.SerialException) as error:
        print(f"waiting for {DEVICE}: {error}", file=sys.stderr)
        time.sleep(0.5)
