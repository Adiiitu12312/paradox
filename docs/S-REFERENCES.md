# S: pending — reference identified after M7

Lookup performed October 16, 2026 (Asia/Calcutta), after M7 acceptance was
committed as ab1e324. S discovery, brainstorming, scope and implementation remain
pending; no cloud-system code or architecture has been selected.

The Claude-related reference is **Scion**, Anthropic's internal version-control
system. The primary source is
[Redesigning source control at Anthropic](https://www.anthropic.com/engineering/redesigning-source-control),
published October 13 and updated October 15, 2026.

Anthropic describes server-side history, merges and rebases, a bounded client
cache, and sparse or virtualized working copies that fetch required data.
It presents Scion as a specialized solution for its unusually large workload,
rather than a general-purpose Git replacement. The article also credits
Sapling's Mononoke server and Bonsai data model as prior work.

The owner also named Meta's EdenFS/Sapling as references. Comparing these
approaches with Paradox's requirements is reserved for the later discussion.
No migration, daemon, storage service or replacement of Git is authorized by
this reference note. M8 and M9 also remain pending.
