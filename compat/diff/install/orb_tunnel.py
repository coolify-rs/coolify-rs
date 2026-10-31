#!/usr/bin/env python3
"""Forwards a local port to a port inside an OrbStack VM, one `orb` process per connection.

This Mac may have no network route to the VMs. The tunnel is only how a browser on the Mac reaches
the VM's published port; everything the installed server does stays inside the VM.

    orb_tunnel.py <machine> <local port> <vm port>
"""
import asyncio
import sys

machine, local_port, vm_port = sys.argv[1], int(sys.argv[2]), sys.argv[3]


async def pump(reader, writer):
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, BrokenPipeError):
        pass
    finally:
        try:
            writer.close()
        except (ConnectionError, RuntimeError):
            pass


async def handle(client_r, client_w):
    proc = await asyncio.create_subprocess_exec(
        "orb", "-m", machine, "-u", "root", "nc", "-q", "1", "127.0.0.1", vm_port,
        stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
    )
    await asyncio.gather(pump(client_r, proc.stdin), pump(proc.stdout, client_w))
    try:
        proc.kill()
    except ProcessLookupError:
        pass
    await proc.wait()


async def main():
    server = await asyncio.start_server(handle, "127.0.0.1", local_port)
    async with server:
        await server.serve_forever()


asyncio.run(main())
