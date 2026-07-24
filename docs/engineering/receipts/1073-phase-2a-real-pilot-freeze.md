# #1073 phase-2a manifest freeze receipt

- Status: real 50-row manifest selected and frozen; no model calls performed.
- Manifest: `1073-phase-2a-real-pilot-manifest.json`
- Contract SHA-256: `5ffb20e6d400b3d96a61ac14e55c6193f00cf83fd24cd1c19839b22a35f62ded`
- Privacy: every selected source passed the production privacy gate before verified save; no source text is persisted.
- Source access: strict read-only SQLite, `query_only`, stable transactions.

| Dimension | Counts |
| --- | --- |
| Rows | 50 |
| Source routes | antigravity 25, hapi 25 |
| Kinds | narrative 25, structured-control 25 |
| Strata | correction/alignment 17, verification/recovery 17, routing/store/provenance 16 |
