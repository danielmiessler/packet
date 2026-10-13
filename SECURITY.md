# Security

Packet runs with your user's permissions, sends only measurement traffic to the hosts listed in the README, and stores numbers about the network path in one local SQLite file. It has no server, no account and no telemetry.

## Reporting a vulnerability

Please report security issues privately through GitHub's **Report a vulnerability** button on this repository's Security tab, not in a public issue. Include the macOS version, the Packet version (`defaults read /Applications/Packet.app/Contents/Info CFBundleShortVersionString`), and steps to reproduce.

You should hear back within a week. Fixes ship in a new release, and the advisory credits you unless you ask otherwise.

## In scope

- Anything that makes Packet send traffic to a host that is not on the README's list.
- Anything that makes Packet record traffic contents, visited hosts, app names or other personal data.
- Code execution, privilege escalation or file writes outside `~/Library/Application Support/Packet` through Packet.
