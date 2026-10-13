# Security model

## Agent-only authentication

SSH processes use BatchMode, publickey-only authentication, IdentityFile=none,
StrictHostKeyChecking=yes and ForwardAgent=no. No password prompt, password
storage, private-key export or arbitrary shell command endpoint exists.
Agent availability does not promise a signature will be authorized. Unlock
and confirmation remain the SSH Agent's responsibility.

## Link boundary

A native deep link is not proof that a trustworthy website sent it. Requests
are handled without an extra manager approval. The URL supplies typed fields,
not shell strings. SSH Agent signing and OpenSSH host verification remain mandatory. The destination
forward is always remote 127.0.0.1 and the listener is local 127.0.0.1.
Complete page paths stay in memory with the connection; no service catalogue
or local secret path files are maintained. Diagnostics exclude complete URLs.

OpenSSH known_hosts is authoritative. When a link specifies a fingerprint,
it must match an existing verified host record. The app never automatically
trusts ssh-keyscan output or silently accepts a changed host key. Initial
trust must be established separately through a verified channel.

The app terminates only child processes it created. Other port listeners are
reported as conflicts. Duplicate requests reuse the owned connection; changed
fingerprints require explicit disconnection and verification.

## Installation

The installer and startup checks touch only this application's protocol
registrations and login-start setting. An explicit disabled preference is
respected. The app does not modify sshd, firewall, endpoint protection,
Bitwarden settings or other applications' registrations.

## Limits

A compromised local computer, authorized malicious portal or approved target
can still cause harm. Agent isolation is not malware protection. Automatic
reconnection never removes host validation or Agent confirmation. Platform
signing and notarization are independent release gates. Never bypass an OS
security warning to run a build.

Report vulnerabilities privately to the repository maintainer using GitHub's
private vulnerability reporting where enabled; do not post credentials or
private service URLs in public issues.