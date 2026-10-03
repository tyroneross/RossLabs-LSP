# Compatibility learning strategy

Use the native `compat record` and query commands with a local registry. Link each
record to Build Loop memory and a regression fixture. Keep standard language
semantics in existing LSP engines; terminal, process, schema, and protocol
compatibility belongs in this companion registry.

## Verification order

1. Identify producer, consumer, capability, versions, and environment.
2. Check schema and negotiated capabilities against the previous contract.
3. Run shared consumer/provider fixtures on both implementations.
4. Exercise properties and state transitions for lifecycle behavior.
5. Verify the user behavior in the actual native host and client.
6. Preserve evidence, limitations, and a regression fixture in the retrospective.

Buf's [breaking-change checks](https://buf.build/docs/breaking/) demonstrate
schema comparison. [Pact provider verification](https://docs.pact.io/provider)
checks provider behavior against consumer contracts. Schemathesis's
[stateful testing](https://schemathesis.readthedocs.io/en/latest/explanations/stateful/)
chains operations with response data. These support this layered approach;
none alone proves complete runtime compatibility.

Use existing tests first. These are design patterns, not required new dependencies.
No performance speedup has been measured for this repository.

## Discovery and learning

Transcript mining supplies candidates. Require an interaction and failure signal,
retain source session/timestamp, deduplicate copied reports, and cluster by
boundary/capability/symptom. Two language names alone do not establish a defect.
A persistent event cursor and content hashes are a proposed efficiency improvement,
not an implemented mining service here.

Every Build Loop Learn run records updated/no-change/not-applicable/deferred.
Updates link immutable registry IDs to a local memory artifact. Deferred or missing
reviews leave Learn incomplete. Historical reports stay reported until reproduced.
Research checked 2026-10-02; schema validation verifies structure, not evidence truth.
