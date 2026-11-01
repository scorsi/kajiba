"""Test-only discoverable ES256 authenticator for the disaster-recovery VM."""

import base64
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import urllib.request


def b64(value):
    return base64.urlsafe_b64encode(value).decode().rstrip("=")


def cbor(value):
    def head(major, length):
        if length < 24:
            return bytes([major * 32 + length])
        assert length < 256
        return bytes([major * 32 + 24, length])

    if isinstance(value, int):
        return head(0, value) if value >= 0 else head(1, -1 - value)
    if isinstance(value, bytes):
        return head(2, len(value)) + value
    if isinstance(value, str):
        return head(3, len(value.encode())) + value.encode()
    return head(5, len(value)) + b"".join(cbor(k) + cbor(v) for k, v in value.items())


def post(path, body):
    request = urllib.request.Request(
        "https://forge/auth/webauthn/" + path,
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Origin": "https://forge"},
    )
    with urllib.request.urlopen(request, timeout=15) as response:
        data = response.read()
        return json.loads(data) if data else response.headers["Set-Cookie"].split(";", 1)[0]


def client_data(options, kind):
    return json.dumps(
        {"type": "webauthn." + kind, "challenge": options["challenge"], "origin": "https://forge", "crossOrigin": False},
        separators=(",", ":"),
    ).encode()


def enroll(directory, token):
    directory.mkdir(mode=0o700)
    key = directory / "key.pem"
    subprocess.run(["openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", str(key)], check=True)
    public = subprocess.check_output(["openssl", "ec", "-in", str(key), "-pubout", "-outform", "DER"], stderr=subprocess.DEVNULL)[-65:]
    assert len(public) == 65 and public[0] == 4
    start = post("enroll/start", {"token": token})
    options = start["options"]["publicKey"]
    credential_id = os.urandom(32)
    (directory / "credential.json").write_text(json.dumps({"id": b64(credential_id), "handle": options["user"]["id"]}))
    # UP + UV + attested credential data; zero counter supports repeat assertions.
    auth = hashlib.sha256(options["rp"]["id"].encode()).digest() + b"\x45" + bytes(4)
    auth += bytes(16) + struct.pack(">H", len(credential_id)) + credential_id
    auth += cbor({1: 2, 3: -7, -1: 1, -2: public[1:33], -3: public[33:]})
    return post("enroll/finish", {
        "ceremony": start["ceremony"], "name": "recovery fixture",
        "credential": {"id": b64(credential_id), "rawId": b64(credential_id), "type": "public-key", "extensions": {},
                       "response": {"clientDataJSON": b64(client_data(options, "create")),
                                    "attestationObject": b64(cbor({"fmt": "none", "attStmt": {}, "authData": auth}))}},
    })


def login(directory):
    credential = json.loads((directory / "credential.json").read_text())
    start = post("login/start", {})
    options = start["options"]["publicKey"]
    client = client_data(options, "get")
    auth = hashlib.sha256(options["rpId"].encode()).digest() + b"\x05" + bytes(4)
    signature = subprocess.check_output(
        ["openssl", "dgst", "-sha256", "-sign", str(directory / "key.pem")],
        input=auth + hashlib.sha256(client).digest(),
    )
    return post("login/finish", {
        "ceremony": start["ceremony"],
        "credential": {"id": credential["id"], "rawId": credential["id"], "type": "public-key", "extensions": {},
                       "response": {"clientDataJSON": b64(client), "authenticatorData": b64(auth),
                                    "signature": b64(signature), "userHandle": credential["handle"]}},
    })


if __name__ == "__main__":
    os.umask(0o077)
    assert cbor({1: -7, -2: b"ab"}) == bytes.fromhex("a2012621426162")
    assert b64(b"\xfb\xff") == "-_8"
    directory = Path(sys.argv[2])
    print(enroll(directory, sys.argv[3]) if sys.argv[1] == "enroll" else login(directory))
