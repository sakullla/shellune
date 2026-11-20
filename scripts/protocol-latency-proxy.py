#!/usr/bin/env python3
"""A bounded localhost TCP delay fixture; does not change the host's network rules.

Example: --listen 15993 --upstream 15991 --delay 3 adds about 6s RTT.
SSH -L can expose the loopback listener to a remote test client.
"""
import argparse
import asyncio
import json
import time


async def relay(reader, writer, delay, counts, name, rate):
    pending = asyncio.Queue(maxsize=16)  # At most 1 MiB per direction.

    async def receive():
        while True:
            data = await reader.read(65536)
            await pending.put((time.monotonic() + delay, data))
            if not data:
                return

    async def send():
        available = 0.0
        while True:
            due, data = await pending.get()
            if not data:
                await asyncio.sleep(max(0, due - time.monotonic()))
                if writer.can_write_eof():
                    writer.write_eof()
                return
            # Preserve propagation delay while pacing large reads into small bursts.
            burst = max(1, rate // 20) if rate else len(data)
            for offset in range(0, len(data), burst):
                part = data[offset:offset + burst]
                available = max(due, available) + (len(part) / rate if rate else 0)
                await asyncio.sleep(max(0, available - time.monotonic()))
                writer.write(part)
                await writer.drain()
                counts[name] += len(part)

    async with asyncio.TaskGroup() as group:
        group.create_task(receive())
        group.create_task(send())


async def main(args):
    async def accept(reader, writer):
        remote_writer = None
        counts = dict(sent=0, received=0)
        started = time.monotonic()
        try:
            remote_reader, remote_writer = await asyncio.open_connection("127.0.0.1", args.upstream)
            # When either direction closes, cancel the other so a disconnected
            # client cannot leave its relay parked on a silent upstream forever.
            rate = args.kbps * 125  # Decimal kilobits per second, per direction.
            tasks = [asyncio.create_task(relay(reader, remote_writer, args.delay, counts, "sent", rate)),
                     asyncio.create_task(relay(remote_reader, writer, args.delay, counts, "received", rate))]
            try:
                await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            finally:
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)
        finally:
            writer.close()
            if remote_writer:
                remote_writer.close()
            print(json.dumps(dict(counts, seconds=round(time.monotonic() - started, 3))), flush=True)

    server = await asyncio.start_server(accept, "127.0.0.1", args.listen)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--listen", type=int, required=True)
    parser.add_argument("--upstream", type=int, required=True)
    parser.add_argument("--delay", type=float, required=True)
    parser.add_argument("--kbps", type=int, default=0, help="per-direction bandwidth in kbit/s (0 = unlimited)")
    args = parser.parse_args()
    if args.delay < 0 or args.kbps < 0:
        parser.error("delay and bandwidth must be nonnegative")
    asyncio.run(main(args))
