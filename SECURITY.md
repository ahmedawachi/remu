# Security policy

Remu grants one machine control of another. Please treat bugs here as security bugs by default.

## Reporting a vulnerability

Open a [private security advisory](https://github.com/remu-app/remu/security/advisories/new)
rather than a public issue. Please include a description, affected version or commit, and the
smallest reproduction you have. We aim to acknowledge within 72 hours.

Please do not test against relays or desks you do not own.

## What we consider a vulnerability

- Obtaining screen access or input control without the host accepting, or without a valid
  unattended-password proof.
- A relay operator, or anyone who can observe relay traffic, recovering the unattended password,
  session media, or transferred file contents.
- A connected peer reading or writing files outside the configured download directory, or escaping
  the permissions the host advertised.
- Crashing or hanging a host or relay with malformed protocol input.
- Any injected input reaching a host that has "allow remote keyboard & mouse" switched off.

## Known limits — by design, not bugs

These are documented trade-offs. Reports about them are welcome as discussion, but they are not
treated as vulnerabilities:

- **The unattended password is not a PAKE.** A host that is successfully impersonated can collect
  HMAC proofs and mount an offline dictionary attack against a weak password. Use a strong one, or
  leave it unset and accept prompts by hand. See [docs/PROTOCOL.md](docs/PROTOCOL.md) §4.
- **The relay learns metadata.** It sees which desk IDs are online and which of them connect to each
  other, along with the source IPs. It cannot read media, files, chat or the password.
- **`AcceptPolicy::Always` disables the human check.** That is what it is for. It is off by default.
- **A compromised host is a compromised host.** Remu does not defend a machine against software
  already running on it with the user's privileges.

## Operating a relay

The relay ships with no TLS of its own. Terminate TLS in front of it (`wss://`) and set `--token` so
it does not hand desk IDs to anyone who connects. Both are one flag and one reverse-proxy block; a
relay exposed to the internet without them is an open directory of reachable desks.
