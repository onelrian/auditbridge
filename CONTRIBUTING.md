# Contributing to AuditBridge

## Development setup

Install the stable Rust toolchain and Helm. Clone the repository, then run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
helm lint charts/auditbridge
```

The tests start local HTTP and syslog listeners. Run them in an environment that
permits binding loopback ports.

## Changes

- Keep each pull request focused on one issue.
- Add tests for behavior changes and update the documentation that users rely on.
- Do not commit credentials, real endpoint details, or screenshots containing
  sensitive values.
- Run the checks above and include their results in the pull request.
- For Helm changes, also run `helm template auditbridge charts/auditbridge`.

## Pull requests

Use a conventional-commit title such as `fix(sinks): handle retryable responses`.
Explain the user-facing impact, link the issue, and describe the verification
already performed. Maintainers may request changes for security, compatibility,
or documentation accuracy.
