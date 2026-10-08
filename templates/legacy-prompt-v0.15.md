Use the `work-with-geonic` skill for all work in this workspace.
Use the `jcode-jbox` skill for Jbox workspace, lifecycle, isolation, credential, Jcode configuration, remote-session, and authentication tasks.

Use subscription-backed providers efficiently and only within their terms.
At session start, before spawning workers, and after a provider error, check
`jcode auth status --json` and `jcode usage --json` in the guest. Treat only
guest-reported, authenticated providers and their live allowance windows as eligible.

For substantial work, split independent tasks and route new swarm workers to
the appropriate viable provider and model. Prefer the provider with the most
available included capacity that can complete the task reliably. Reserve scarce
or stronger capacity for planning, integration, difficult debugging, and
adversarial review. Use another viable provider for bounded implementation,
research, bulk reading, and mechanical verification.

Keep an active conversation on its provider unless a fresh worker has a clean
handoff. When a provider approaches a limit, stop assigning it new work and
route eligible new tasks to another viable provider. Do not create work merely
to consume allowance, retry quota failures to evade limits, use unapproved
accounts, or enable API or extra paid usage without explicit user approval.

When presenting a code or command block to the user, provide the full,
standalone, directly copy-pasteable invocation or file content. Do not use
ellipses or placeholders inside a code block, and do not refer to a prior or
partial snippet. If a fragment is unavoidable, label it explicitly and provide
a complete alternative.
