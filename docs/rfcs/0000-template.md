# RFC-NNNN: Title

- **Status:** Draft
- **Author(s):** <!-- GitHub usernames -->
- **Created:** YYYY-MM-DD
- **Last updated:** YYYY-MM-DD

---

## Summary

One paragraph. What is being proposed and why it matters.

## Motivation

What problem does this solve? What is the current situation and why is it
unsatisfactory? Include concrete examples where possible.

## Guide-level explanation

Explain the proposal as if you were documenting it for a new contributor.
Focus on *what* changes and *how* users or implementors interact with it.
Avoid low-level detail here, that goes in the next section.

## Reference-level explanation

The technical specification. Be precise. Include:

- Data structures and their ownership model.
- Algorithms and their complexity.
- Interactions with existing subsystems.
- Any WGSL shader changes (if applicable).
- Any changes to the `byld` grammar (if applicable).

## Drawbacks

Why should we *not* do this? What are the costs, risks, or trade-offs?

## Rationale and alternatives

- Why is this design the best among the options considered?
- What other designs were evaluated and why were they rejected?
- What is the impact of *not* doing this?

## Prior art

What has been done in other projects (wgpu ecosystem, other UI frameworks,
game engines, compiler toolchains)? What can we learn from them?

## Resolved questions

**An RFC ships no open questions.** Every question this design raised is
answered here, before merge, not filed as a to-do for whoever implements it.
A deferred question is a decision made later, by someone with less context,
under more pressure, and usually by accident.

For each one, state the question, the options that were weighed, and the
resolution *with its reasoning*. The reasoning is the part that matters: a bare
answer cannot be re-evaluated when circumstances change, and the act of writing
it out is what surfaces the questions that turn out not to be details. (RFC-0030
§Q8 is the canonical example: "do these two scopes nest?" looked like
bookkeeping and turned out to be a bug in an accessor RFC-0013 already shipped.)

If a question genuinely cannot be answered without building something first,
that is a signal to spike it and come back, not to merge the RFC with a
placeholder.

### Q1, one-line statement of the question

**Question.** What exactly is undecided, and why it matters.

**Options.** (a) …; (b) …; (c) ….

**Resolution: (b).** Why, in enough detail that a reader who disagrees can tell
which part of the argument they disagree with.

---

Decisions that only surface *after* merge, implementation-time trade-offs the
design could not have anticipated, go to `support/DESICIONS.md` as `IMPL-NN`
entries, per that file's own rule. They do not come back into the RFC.

## Future possibilities

What natural extensions or follow-ups does this design enable?
This section is not a commitment.
