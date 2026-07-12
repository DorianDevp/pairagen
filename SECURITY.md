# Security policy

## Supported versions

Security fixes are provided for the latest released version.

## Reporting a vulnerability

Please use GitHub private vulnerability reporting for security issues. Do not
open a public issue containing credentials, proprietary source code, session
traces, or an exploitable proof of concept.

Include the affected version, operating system, Neovim version, backend, impact,
and minimal reproduction steps. Redact all private project content.

## Local data and downloaded binaries

Pairagen sends the selected context to the backend configured by the user.
Review that backend's data policy before using it with private code.

Session traces are stored locally. Source content is redacted by default and
retention is bounded, but users can explicitly enable full-content traces.

Managed `paird` binaries are downloaded from versioned GitHub releases and are
executed only after their SHA-256 checksum matches the published checksum file.
