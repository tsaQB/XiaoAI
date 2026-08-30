# Required `master` protection

The repository-side files can harden workflows and ownership, but branch protection/rulesets are GitHub-hosted settings and are not configured by the application. The repository owner should configure `master` with the following minimum policy:

- Require changes through pull requests.
- Require the checks produced by `.github/workflows/build.yml`: **Rust quality gates**, **RustSec dependency audit**, **Linux ARM64 (Armbian)**, and **Android ARM64**.
- Block force pushes.
- Block branch deletion.
- Keep required approvals compatible with a single-maintainer repository; do not require an approval that the only maintainer cannot satisfy.
- Do not allow bypasses for ordinary pushes except an intentional emergency administrator policy.

`CODEOWNERS` establishes ownership metadata but does not by itself enforce review. GitHub branch protection or a repository ruleset must enforce the PR/check requirements above.
