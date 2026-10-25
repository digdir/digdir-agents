# AGENTS.md

Follow [CONTRIBUTING.md](CONTRIBUTING.md).

- Read the code, not the docs, to learn how something works.
- Run `make fmt lint build test` before reporting a change complete.
- Before pushing to a pull request's branch, fetch its base branch and rebase on it.

## Writing

This applies to what you write for others to read, such as:

- docs and skills
- code comments
- commit messages
- issue and pull request descriptions and comments

It does not apply to your conversation with the person prompting you.

- Say each thing once, and link to where it is already said. That includes the code: a version, a default or a list
  of tools belongs where it is defined.
- State the rule _and_ why it exists, not a list of the cases it covers. Such lists go stale as the code changes.
- Describe things by what they do for the reader, not by how they are built today.
- Write the way you would say it to a colleague. Use periods rather than semicolons or em dashes.
- When you fix a problem in a text, check the rest of it for the same problem.
- Write for the reader, not as a record of your work. Leave out the steps you took, what you tried first, who found
  what and lists the diff already shows, such as every test you added. The person prompting you has already seen
  your process. The reviewer has the diff. What they need from you is what it can't show, such as why the change
  looks like this. Notes on your process dilute that.
- When more work changes a pull request, rewrite its description to describe the change as it now is, instead of
  appending to it. Appended rounds turn it into a history the reviewer has to replay to learn the result. A short
  comment can say what changed since the last review.
