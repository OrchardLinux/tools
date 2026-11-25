#!/usr/bin/env python3
"""Makes the inputs in this directory and prints what orchard's Python
extractors give for them, which tests/steps.rs expects of the Rust ones.

    PYTHONPATH=<orchard checkout>/src python3 make.py

The inputs are made up: small, and shaped like the real files only as far as
the extractors look. Run it again when a Python extractor changes, and copy
the printed values into tests/steps.rs.
"""
import hashlib
import plistlib
import struct
from pathlib import Path

from orchard.apple import bluetool, sgx, smartio, z2packet

HERE = Path(__file__).parent


def sha(data):
    return hashlib.sha256(data).hexdigest()


def xe32(value):
    return struct.pack('>HH', value & 0xffff, value >> 16)


def z2_packet(address, payload):
    """A prefixed Z2 packet whose word count covers every payload word."""
    header = struct.pack('>H', len(payload) // 4) + xe32(address)
    body = z2packet.PREFIX + header + struct.pack('>H', sum(header)) + payload
    return body + xe32(sum(payload) & 0xffffffff)


def segment64(name, vm, vmsize, offset, size, sections=()):
    out = struct.pack('<II16s4Q4I', 0x19, 72 + 80 * len(sections), name, vm, vmsize, offset, size,
                      7, 7, len(sections), 0)
    for section, address, length, at in sections:
        out += struct.pack('<16s16sQQ8I', section, name, address, length, at, 0, 0, 0, 0, 0, 0, 0)
    return out


def macho64(filetype, commands):
    body = b''.join(commands)
    return struct.pack('<8I', 0xfeedfacf, 0x100000c, 0, filetype, len(commands), len(body), 0, 0) + body


def segment32(name, vm, vmsize, offset, size, sections=()):
    out = struct.pack('<II16s4I4I', 1, 56 + 68 * len(sections), name, vm, vmsize, offset, size,
                      7, 7, len(sections), 0)
    for section, address, length in sections:
        out += struct.pack('<16s16s9I', section, name, address, length, 0, 0, 0, 0, 0, 0, 0)
    return out


def macho32(commands):
    body = b''.join(commands)
    return struct.pack('<7I', 0xfeedface, 12, 9, 2, len(commands), len(body), 0) + body


# N1 touch: one packet (@2), and a list of packets (@3).
first = z2_packet(0, bytes(range(64)))
second = z2_packet(0x2000, bytes(range(64, 96)))
mtprops = plistlib.dumps({
    'One,1': {'PreconstructedBootloadPacketType': 'Z2', 'Constructed Firmware': first},
    'Two,1': {'PreconstructedBootloadPacketType': 'Z2', 'Constructed Firmware': [first, second]},
    'Config,1': {'PreconstructedBootloadPacketType': 'Z2', 'Constructed Firmware': first,
                 'Firmware Config': b'<dict></dict>'},
})
(HERE / 'n1.mtprops').write_bytes(mtprops)
N1 = (0x10008000, 1024, 0x10008600, 128)
print('n1 one  ', sha(z2packet.convert_n1(mtprops, 'One,1', *N1)))
print('n1 list ', sha(z2packet.convert_n1_packets(mtprops, 'Two,1', *N1)))

# BlueTool: a table of 210 bytes in the second of two segments.
table = bytes((7 * i) & 0xff for i in range(210))
commands = [segment64(b'__TEXT', 0x100000000, 0x1000, 0, 0x400),
            segment64(b'__DATA', 0x100001000, 0x1000, 0x400, 0x200)]
tool = macho64(2, commands).ljust(0x400, b'\0') + bytes(0x80) + table
tool = tool.ljust(0x600, b'\0')
(HERE / 'bluetool.macho').write_bytes(tool)
print('regulatory', sha(bluetool.regulatory_from_binary(tool, 0x100001080, 210)), sha(table))

# SGX543: a 32-bit kernel with two prelinked kexts, one of them the driver.
strings, data, const = b'IMGSGX543\0SGXDriver543\0'.ljust(32, b'\0'), bytes(range(48)), bytes(range(100, 132))
base, page = 0x80010000, 0x1000


def kext(at, cstring):
    vm = base + at - page
    text = segment32(b'__TEXT', vm, 0x300, 0, 0x300, [(b'__cstring', vm + 0x200, 32), (b'__const', vm + 0x240, 32)])
    rest = segment32(b'__DATA', vm + 0x300, 0x100, 0x300, 0x100, [(b'__data', vm + 0x300, 48)])
    image = macho32([text, rest]).ljust(0x200, b'\0') + cstring + bytes(32) + const
    return image.ljust(0x300, b'\0') + data.ljust(0x100, b'\0')


kernel = macho32([segment32(b'__TEXT', 0x80001000, page, 0, page),
                  segment32(b'__PRELINK_TEXT', base, 2 * page, page, 2 * page)]).ljust(page, b'\0')
kernel += kext(page, b'OtherDriver\0'.ljust(32, b'\0')).ljust(page, b'\0')
kernel += kext(2 * page, strings).ljust(page, b'\0')
(HERE / 'kernel32.macho').write_bytes(kernel)
print('sgx     ', sha(sgx.extract(kernel, '12H321')))

# SmartIO in a kernel that merges its kexts: the kext has a ModuleIndex and
# no load address, and the firmware is the kernel's own section.
info = (b'<dict><key>_PrelinkInfoDictionary</key><array><dict>'
        b'<key>CFBundleIdentifier</key><string>com.apple.driver.AppleS5L8960XSmartIO</string>'
        b'<key>ModuleIndex</key><integer size="32">0x7</integer></dict></array></dict>\0')
firmware = bytes((i * 31) & 0xff for i in range(4096))
vm, where = 0xfffffff007004000, 0x1000
commands = [segment64(b'__DATA', vm, 0x1000, where, 0x1000, [(b'__firmware', vm, 4096, where)]),
            segment64(b'__PRELINK_INFO', vm + 0x1000, 0x1000, where + 0x1000, 0x1000,
                      [(b'__info', vm + 0x1000, len(info), where + 0x1000)])]
merged = macho64(2, commands).ljust(where, b'\0') + firmware + info.ljust(0x1000, b'\0')
(HERE / 'merged.kernel').write_bytes(merged)
print('smartio ', sha(smartio.extract(merged, smartio.S5L8960X)[0]), sha(firmware))
