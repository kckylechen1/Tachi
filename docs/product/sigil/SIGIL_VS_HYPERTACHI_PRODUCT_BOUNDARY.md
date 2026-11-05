---
title: "Tachi and Hyperion Product Boundary"
summary: "Shared memory primitives, distinct host responsibilities, and repository-owned documentation."
category: "product/sigil"
---
# Tachi and Hyperion product boundary

**Status:** Current boundary; reviewed 2026-11-04. The Sigil and HyperTachi
names in this path are historical product names.

Tachi supplies shared memory primitives and its agent work-control product.
Hyperion supplies finance-specific ingestion, interpretation and trading
workflows. A shared kernel does not require the two hosts to load the same
instructions, skills, domain logic or live databases.

## Repository documentation

`tachi_wiki_organize` is retired. Its classifier, file-moving pipeline,
frontmatter publication and task-checkbox synchronization are removed.
Repository documentation is maintained by the host's normal file tools under
the repository's own instructions and Git history.

Tachi retains Wiki storage, search, read, ingestion and lifecycle operations.
Skill catalog management, static document access and export remain available;
they do not grant authority to rewrite a repository's agent instructions.

## Host-specific knowledge

Hyperion owns its research-report ingestion, trading lessons, decision recall
and finance-specific defaults. Tachi's memory kernel remains domain-neutral.
Repository specifications and agent contracts remain Git-managed sources;
memory may retain pointers, evidence and conclusions without replacing them.

See [the host substrate](../../engineering/architecture/agent-host-substrate.md)
and [the Cloud settings draft](../../engineering/architecture/tachi-cloud-agent-network-future-spec.md)
for the separate host-integration and future portability boundaries.
