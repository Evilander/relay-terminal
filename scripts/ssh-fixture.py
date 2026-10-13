"""Finite localhost SSH/SFTP server used by native integration tests.

Requires Paramiko. All keys, trust files and server files are generated beneath
the explicitly supplied output directory; the user's SSH files are untouched.
"""
import argparse
import errno
import hashlib
import json
import logging
import os
from pathlib import Path
import posixpath
import socket
import stat
import subprocess
import threading
import time

import paramiko

logging.getLogger("paramiko").setLevel(logging.CRITICAL)


class FileHandle(paramiko.SFTPHandle):
    def __init__(self, flags, read_limit=None, changed_metadata=False, mutate_source=None):
        super().__init__(flags)
        self.read_limit = read_limit
        self.changed_metadata = changed_metadata
        self.mutate_source = mutate_source
        self.has_read = False

    def read(self, offset, length):
        if self.read_limit is not None:
            if offset >= self.read_limit:
                return b""
            length = min(length, self.read_limit - offset)
        result = super().read(offset, length)
        if isinstance(result, bytes) and result:
            self.has_read = True
        return result

    def write(self, offset, data):
        if self.mutate_source is not None:
            self.mutate_source.write_bytes(b"")
            self.mutate_source = None
        return super().write(offset, data)

    def stat(self):
        try:
            stream = getattr(self, "readfile", None) or self.writefile
            attributes = paramiko.SFTPAttributes.from_stat(os.fstat(stream.fileno()))
            if self.changed_metadata and self.has_read:
                attributes.st_mtime += 1
            return attributes
        except OSError as error:
            return paramiko.SFTPServer.convert_errno(error.errno)


class Files(paramiko.SFTPServerInterface):
    def __init__(self, server, root):
        super().__init__(server)
        self.server = server
        self.root = Path(root).resolve()

    def local(self, path):
        relative = posixpath.normpath("/" + path).lstrip("/")
        candidate = (self.root / relative).resolve()
        candidate.relative_to(self.root)
        return candidate

    def canonicalize(self, path):
        relative = self.local(path).relative_to(self.root).as_posix()
        return "/" if relative == "." else "/" + relative

    def list_folder(self, path):
        try:
            if self.local(path) == self.root / "virtual-large-list":
                entries = []
                for index in range(3000):
                    entry = paramiko.SFTPAttributes()
                    entry.filename = "x" * 4096 + str(index)
                    entry.st_mode = stat.S_IFREG | 0o600
                    entry.st_size = 1
                    entries.append(entry)
                return entries
            entries = []
            for child in self.local(path).iterdir():
                entry = paramiko.SFTPAttributes.from_stat(child.stat())
                entry.filename = child.name
                entries.append(entry)
            return entries
        except (OSError, ValueError):
            return paramiko.SFTP_NO_SUCH_FILE

    def stat(self, path):
        try:
            return paramiko.SFTPAttributes.from_stat(self.local(path).stat())
        except (OSError, ValueError):
            return paramiko.SFTP_NO_SUCH_FILE

    lstat = stat

    def open(self, path, flags, attr):
        try:
            local_path = self.local(path)
            fd = os.open(local_path, flags | getattr(os, "O_BINARY", 0), 0o600)
            mode = "r+b" if flags & os.O_RDWR else "wb" if flags & os.O_WRONLY else "rb"
            stream = os.fdopen(fd, mode)
            handle = FileHandle(
                flags,
                read_limit=65536 if local_path.name == "premature.bin" else None,
                changed_metadata=local_path.name == "changed.bin",
                mutate_source=self.root.parent / "unstable-upload.bin"
                if local_path.parent == self.root / "mutating-upload" else None,
            )
            if flags & os.O_RDWR or not flags & os.O_WRONLY:
                handle.readfile = stream
            if flags & (os.O_WRONLY | os.O_RDWR):
                handle.writefile = stream
            return handle
        except (OSError, ValueError) as error:
            return paramiko.SFTPServer.convert_errno(getattr(error, "errno", errno.EACCES))

    def remove(self, path):
        try:
            self.local(path).unlink()
            return paramiko.SFTP_OK
        except (OSError, ValueError) as error:
            return paramiko.SFTPServer.convert_errno(getattr(error, "errno", errno.EACCES))

    def rename(self, old, new):
        try:
            source, target = self.local(old), self.local(new)
            if target.exists():
                return paramiko.SFTP_FAILURE
            source.rename(target)
            if target.name.startswith("lost-ack-"):
                self.server.transport.close()
            return paramiko.SFTP_OK
        except (OSError, ValueError) as error:
            return paramiko.SFTPServer.convert_errno(getattr(error, "errno", errno.EACCES))


class Server(paramiko.ServerInterface):
    def __init__(self, public_key):
        self.public_key = public_key
        self.shell = threading.Event()

    def check_auth_publickey(self, username, key):
        return paramiko.AUTH_SUCCESSFUL if username == "relay-test" and key == self.public_key else paramiko.AUTH_FAILED

    def get_allowed_auths(self, username):
        return "publickey"

    def check_channel_request(self, kind, channel_id):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_pty_request(self, channel, term, width, height, pixelwidth, pixelheight, modes):
        return True

    def check_channel_window_change_request(self, channel, width, height, pixelwidth, pixelheight):
        return True

    def check_channel_shell_request(self, channel):
        self.shell.set()
        return True


def serve(client, host_key, identity, root, deadline):
    transport = paramiko.Transport(client)
    try:
        transport.add_server_key(host_key)
        transport.set_subsystem_handler("sftp", paramiko.SFTPServer, Files, root=root)
        server = Server(identity)
        server.transport = transport
        transport.start_server(server=server)
        channel = transport.accept(10)
        if channel is None:
            return
        while transport.is_active() and time.monotonic() < deadline:
            if server.shell.wait(0.1):
                channel.send("Relay SSH fixture ready — café λ\r\nfixture> ".encode())
                channel.settimeout(0.5)
                while transport.is_active() and time.monotonic() < deadline:
                    try:
                        command = channel.recv(8192)
                        if not command:
                            break
                        if b"exit" in command:
                            channel.send_exit_status(0)
                            return
                        channel.send(b"received: " + command.replace(b"\r", b"\r\n") + b"\r\nfixture> ")
                    except socket.timeout:
                        continue
                break
    except (EOFError, OSError, paramiko.SSHException):
        pass
    finally:
        transport.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("--seconds", type=int, default=1200)
    args = parser.parse_args()
    if not args.output.is_absolute() or not 1 <= args.seconds <= 1800:
        parser.error("Use an absolute output directory and a duration of 1..1800 seconds")
    args.output.mkdir(parents=True, exist_ok=False)
    remote = args.output / "remote"
    remote.mkdir()
    (remote / "nested").mkdir()
    (remote / ".hidden").mkdir()
    (remote / ".hidden" / "visible.txt").write_text("Hidden directory fixture\n", encoding="utf-8")
    (remote / "hello café.txt").write_text("Hello over SFTP: café λ 日本語\n", encoding="utf-8")
    payload = bytes(range(256)) * 4096
    (remote / "payload.bin").write_bytes(payload)
    (remote / "premature.bin").write_bytes(payload)
    (remote / "changed.bin").write_bytes(payload)
    (remote / "mutating-upload").mkdir()
    (remote / "virtual-large-list").mkdir()
    (args.output / "upload.bin").write_bytes(payload[::-1])
    (args.output / "unstable-upload.bin").write_bytes(payload)
    identity = paramiko.RSAKey.generate(2048)
    key_path = args.output / "identity"
    identity.write_private_key_file(str(key_path))
    key_path.chmod(0o600)
    if os.name == "nt":
        environment = {**os.environ, "RELAY_FIXTURE_KEY": str(key_path)}
        subprocess.run([
            "powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
            "$ErrorActionPreference = 'Stop'; "
            "$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User; "
            "$acl = New-Object System.Security.AccessControl.FileSecurity; "
            "$acl.SetOwner($sid); $acl.SetAccessRuleProtection($true, $false); "
            "$acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'Allow'))); "
            "[System.IO.File]::SetAccessControl($env:RELAY_FIXTURE_KEY, $acl)",
        ], env=environment, check=True, timeout=15)
    host_key = paramiko.RSAKey.generate(2048)
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(8)
    listener.settimeout(0.5)
    port = listener.getsockname()[1]
    known_hosts = args.output / "known_hosts"
    known_hosts.write_text(f"[127.0.0.1]:{port} {host_key.get_name()} {host_key.get_base64()}\n")
    config = args.output / "ssh_config"
    config.write_text(f'Host *\n  UserKnownHostsFile "{known_hosts.as_posix()}"\n  GlobalKnownHostsFile none\n  IdentityFile "{key_path.as_posix()}"\n  IdentitiesOnly yes\n  StrictHostKeyChecking yes\n', encoding="utf-8")
    profile = {"id":"fixture", "name":"Local SSH fixture", "folder":"Tests", "host":"127.0.0.1", "port":port, "user":"relay-test", "keyPath":str(key_path), "jumpHost":None}
    details = {"profile":profile, "sshConfig":str(config), "root":str(remote), "payloadSha256":hashlib.sha256(payload).hexdigest(), "seconds":args.seconds}
    (args.output / "fixture.json").write_text(json.dumps(details, indent=2))
    print(json.dumps({"port":port,"fixture":str(args.output / "fixture.json")}), flush=True)
    deadline = time.monotonic() + args.seconds
    try:
        while time.monotonic() < deadline:
            try:
                client, _ = listener.accept()
            except socket.timeout:
                continue
            threading.Thread(target=serve, args=(client, host_key, identity, remote, deadline), daemon=True).start()
    finally:
        listener.close()


if __name__ == "__main__":
    main()
