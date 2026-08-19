#!/usr/bin/env python3
# A Docker socket in front of the real one that refuses to pull images: each request to
# `/images/create` gets 403, every other request goes through. The evidence scripts of real
# runs point iunctus at it with DOCKER_HOST, so that a run without network cannot make the
# Docker daemon, which has network, fetch an image.
#
# Use it as a context manager: `with NoPull(folder) as socket: ...` gives the path of the socket.
#
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import asyncio
import os
import re
import threading
from pathlib import Path

DOCKER = "/var/run/docker.sock"
REFUSAL = (b"HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: 63\r\n"
           b"Connection: close\r\n\r\n"
           b'{"message":"pull access denied: real runs here pull no images"}')
PULL = re.compile(rb"^POST /(v[0-9.]+/)?images/create", re.I)


async def head(reader):
    """The head of an HTTP message, or None at the end of the stream."""
    try:
        return await reader.readuntil(b"\r\n\r\n")
    except asyncio.IncompleteReadError:
        return None


def header(message, name):
    """The value of a header of an HTTP head, or None."""
    match = re.search(rb"\r\n" + name + rb":\s*([^\r\n]*)", message, re.I)
    return match.group(1).strip().lower() if match else None


async def body(reader, writer, message, response):
    """Copies the body of an HTTP message; False when the stream ends with it."""
    length = header(message, b"content-length")
    if header(message, b"transfer-encoding") == b"chunked":
        while True:
            line = await reader.readuntil(b"\r\n")
            writer.write(line)
            size = int(line.split(b";")[0], 16)
            data = await reader.readexactly(size + 2)
            writer.write(data)
            await writer.drain()
            if size == 0:
                return True
    if length is not None:
        writer.write(await reader.readexactly(int(length)))
        await writer.drain()
        return True
    if response and not re.match(rb"HTTP/1\.[01] (1..|204|304)", message):
        # A response without a length ends with the connection.
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
        return False
    return True


async def pipe(reader, writer):
    while data := await reader.read(65536):
        writer.write(data)
        await writer.drain()
    writer.close()


async def serve(client_reader, client_writer):
    upstream_reader, upstream_writer = await asyncio.open_unix_connection(DOCKER)
    try:
        while (request := await head(client_reader)) is not None:
            if PULL.match(request):
                client_writer.write(REFUSAL)
                await client_writer.drain()
                return
            upstream_writer.write(request)
            await body(client_reader, upstream_writer, request, False)
            response = await head(upstream_reader)
            if response is None:
                return
            client_writer.write(response)
            await client_writer.drain()
            if response.startswith((b"HTTP/1.1 101", b"HTTP/1.0 101")):
                # An attached stream: both ways, as they are, until one side ends.
                await asyncio.gather(pipe(client_reader, upstream_writer),
                                     pipe(upstream_reader, client_writer))
                return
            # The response to HEAD has a head only.
            if request.startswith(b"HEAD "):
                continue
            if not await body(upstream_reader, client_writer, response, True):
                return
    except (ConnectionError, asyncio.IncompleteReadError):
        pass
    finally:
        client_writer.close()
        upstream_writer.close()


class NoPull:
    """Runs the socket in a thread for the time of a `with` block."""

    def __init__(self, folder):
        self.path = str(Path(folder) / "docker.sock")

    def __enter__(self):
        self.loop = asyncio.new_event_loop()
        ready = threading.Event()

        def run():
            asyncio.set_event_loop(self.loop)
            server = self.loop.run_until_complete(asyncio.start_unix_server(serve, self.path))
            self.server = server
            ready.set()
            self.loop.run_forever()

        self.thread = threading.Thread(target=run, daemon=True)
        self.thread.start()
        ready.wait()
        return self.path

    def __exit__(self, *_):
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(timeout=5)
        if os.path.exists(self.path):
            os.unlink(self.path)
