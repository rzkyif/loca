# Contributing

Thank you for your interest in tomat. This document explains how the project
accepts contributions and the most useful ways to help.

## How code reaches the project

All code in this repository is written and committed by the project's
maintainers. At present that is a single maintainer
([@rzkyif](https://github.com/rzkyif)); the group may grow over time.

External code is not accepted through pull requests. Any pull request that
contains code changes will be closed without review. This is a permanent policy.
Please do not invest time in a code pull request.

The reasoning is straightforward:

- A consistent code style and architecture is easier to maintain when every line
  is written by the same small group.
- Reviewing external code is a cost the project has chosen not to carry.
  Generating an implementation from a clear description is now inexpensive. A
  clear description of the problem, and of what a good solution would do, is the
  part that genuinely needs human effort.

The project therefore asks contributors to invest in problem descriptions,
experiments, and findings rather than in finished code.

## How you can help

There are three ways to contribute, in roughly increasing order of effort.

### 1. Report bugs

Test the application and report anything that does not work as expected. Open a
[bug report](https://github.com/rzkyif/tomat/issues/new?template=bug-report.yml)
and include:

- **Affected section.** Which part of the app the bug occurs in.
- **Steps to reproduce.** The minimal sequence that triggers it.
- **Expected result.** What you expected to happen.
- **Actual result.** What happened instead.

Search existing issues first. A comment on an existing report is more useful
than a duplicate.

### 2. Request features or improvements

The project distinguishes two kinds of request:

- A **feature request** asks for a new capability that Tomat does not have
  today.
- An **improvement** asks for a refinement to a feature that already exists,
  such as reducing friction, clarifying copy, or adding a missing shortcut.

Choose the matching form so the request lands in the right place:

- [Feature requests](https://github.com/rzkyif/tomat/discussions/new?category=feature-requests)
- [Improvement suggestions](https://github.com/rzkyif/tomat/discussions/new?category=improvement-suggestions)

Describe the problem and the outcome you want. A concrete use case is more
useful than a proposed implementation.

### 3. Publish implementation experiments

If you want to go further, you are welcome to experiment with an implementation
on your own fork. Do not open a pull request with the result. Instead, publish
what you learned in the relevant feature request or improvement discussion:

- the approach you tried, and why,
- what worked and what did not,
- trade-offs, edge cases, and constraints you found,
- pointers to the relevant code in your fork.

Findings of this kind let a maintainer reimplement the feature directly, with
full context, while keeping the repository's code style and structure
consistent.

## Security issues

Do not report security-sensitive issues through public issues or discussions.
See [SECURITY.md](SECURITY.md) for the private disclosure process.

## Running from source

To test pre-release builds or unreleased changes, follow the setup instructions
in [DEVELOPMENT.md](DEVELOPMENT.md#setup):

```bash
deno install
deno task dev
```
