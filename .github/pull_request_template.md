## Summary

Provide a brief summary of the changes in this pull request and the problem being solved.

## Contract Change Safety Checklist

Please verify that your changes adhere to contract stability requirements:

- [ ] **Event Shapes:** Does this PR modify event topic tuples or data shapes? *(Breaking change per `docs/EVENTS.md`)*
- [ ] **Storage Layout:** Does this PR change storage keys or layout? *(Assessed for archival & migration risks)*
- [ ] **Error Variants:** Does this PR add or renumber contract error codes? *(Client-visible breaking change)*
- [ ] **Changelog:** Has a corresponding entry been added to `CHANGELOG.md`?
- [ ] **Deployments:** Has any impact on deployed contracts or `DEPLOYMENTS.md` been documented?
- [ ] **Verification:** Has this change been tested locally (`cargo test`) and/or exercised on Soroban testnet?

## Related Issues

Closes #
