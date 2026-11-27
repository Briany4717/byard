# Three applications nobody on the project wrote

Step 24 of [the roadmap](roadmap.md). Before an edition is named 1.0, three people who did not
write byld build an application in it, and each moves it to the next edition. Everything
else in the roadmap is checked by the project against what the project thinks the language is;
this is the one check against what somebody else thinks it is.

## What is ready

- **The migration, over the repository's own projects.** `migration.rs` (in
  `crates/byard-cli/tests/it/`) copies every project in the two `examples` directories, runs
  `byard fix` and then `byard check` on each, and fails if one stops checking in the newest
  edition or `fix` does not settle. It runs in `cargo test`. Seventy-six projects pass. What
  breaks there is what would have broken for an outsider, so it is the cheap half of this step.
- **The guide and the reference**, whose every sample is checked, as the only things a newcomer
  is given.
- **A suite with no gap**, so what a newcomer meets that the language does not do is a bug and
  not a known hole.

## What needs people

Three applications, each by a person who has not worked on byld, each different in kind: one with
data from outside (a reply with a type), one of several screens that share state (a `store`), one
that draws (canvas, motion, styles). The person is given the guide and the reference and nothing
else: not the roadmap, not the proposals, not the source.

For each:

1. They write it, in the newest edition, and say aloud what they expected at each place they
   stopped. Someone from the project watches and says nothing.
2. They move it with `byard fix` to the edition after, if there is one, or the project writes
   a throwaway next edition for the trial.
3. What was not as they expected is written down, one line each, in the form of the
   template below.

| | |
|---|---|
| **What they wrote** | The line, or the thing they tried |
| **What they expected** | One sentence |
| **What happened** | The diagnostic, or what was drawn |
| **Whose it was** | The language, the diagnostic, the guide, or the person |

## What counts as done

- Each application checks clean, with no warning, in the newest edition, and draws what its author
  meant.
- Every line whose owner was "the language" or "the diagnostic" is a case in the suite (a gap
  first, then a fix), or a sentence in the reference that says why it is as it is.
- Every line whose owner was "the guide" is fixed in the guide.
- The edition is not named 1.0 until the list is empty.

## What the project must decide

Who the three people are, and how they are asked: the project cannot choose them, because the
point is that it does not know them. Nothing in the repository does it.
