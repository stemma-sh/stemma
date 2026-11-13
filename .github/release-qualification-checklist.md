## Release qualification checklist

Approve the `release` deployment only when every item below is done on the
exact artifacts of this workflow run. Approving starts irreversible steps:
the version tag, npm, PyPI, and the GitHub release. If any item is open,
reject the deployment; nothing has been tagged or published yet.

- [ ] Downloaded this run's `candidate-manifest` and the native artifact for
      the qualification platform (not a local build, not another run).
- [ ] Ran the natural-language workflow in `demo/README.md` through the
      promoted path-based MCP setup, using that exact binary.
- [ ] The source document's hash is unchanged and the output is a new file.
- [ ] Desktop Microsoft Word opens the output without a repair prompt.
- [ ] The tracked replacement is attributed to `Demo Reviewer`.
- [ ] Accept all in Word matches `demo/accepted.txt`; Reject all matches
      `demo/rejected.txt`.
- [ ] The result agrees with the Word visual frozen in the release commit.
- [ ] The qualification report (run id, artifact, Word version, results) is
      recorded before approving.
