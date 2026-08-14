# Security Policy

## Supported versions

| Version | Supported |
| --- | --- |
| 0.2.x | Yes |
| < 0.2 | No |

Fixes land on the latest release. Older versions are not backported.

## Reporting a vulnerability

Report privately through GitHub:
**[Security → Report a vulnerability](https://github.com/suiflex/websift/security/advisories/new)**

Please do not open a public issue for a security problem. A public issue is visible to everyone
before a fix exists.

Expect an acknowledgement within 7 days. If a report is confirmed, you will get an estimate for
the fix and credit in the advisory unless you ask otherwise.

## What counts as a vulnerability

Websift's job is to fetch untrusted content under strict limits, so anything that escapes those
limits is in scope:

- **Bypassing the public-destination guard.** Reaching a private, loopback, link-local, or
  otherwise reserved address — including through DNS rebinding, redirects, or a crafted URL.
- **Bypassing the robots gate.** Fetching a path that `robots.txt` disallows, or treating an
  unreadable `robots.txt` as permission.
- **Cross-profile leakage.** One profile reading or writing another profile's crawl jobs,
  documents, or page cache.
- **Self-update integrity.** Installing a binary whose checksum does not match the one published
  beside it, or redirecting the update to an attacker-controlled source.
- **Escaping a bound.** Response size, extracted characters, crawl depth or pages, redirect hops,
  or wall-clock budgets — anything that lets remote content force unbounded work.
- **Escaping the worker spool**, or worker output reaching the MCP process stdout.
- Memory-safety or denial-of-service reachable from remote content.

## What does not

- Vulnerabilities in a SearXNG instance you configured. Report those upstream.
- Rate limiting or blocking by a site you crawled. That is the site's policy, not a defect.
- Findings that require an attacker who already controls the machine running Websift, or who can
  already set its environment variables.
- Missing rendering features. The browser worker is a documented stub, not a security control.
