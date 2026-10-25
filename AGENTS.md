# AGENTS.md

Follow [CONTRIBUTING.md](CONTRIBUTING.md).

- Read the code, not the docs, to learn how something works.
- Run `make fmt lint build test` before reporting a change complete.
- Before pushing to a pull request's branch, fetch its base branch and rebase on it.

## Writing

This applies to docs, skills, pull request descriptions and comments, and commit messages.

- Say each thing once, and link to where it is already said. That includes the code: a version, a default or a list
  of tools belongs where it is defined.
- State the rule _and_ why it exists, not a list of the cases it covers. Such lists go stale as the code changes.
- Describe things by what they do for the reader, not by how they are built today.
- Write the way you would say it to a colleague. Use periods rather than semicolons or em dashes.
- When you fix a problem in a text, check the rest of it for the same problem.
- Write for the reader, not as a record of your work. Leave out the steps you took, what you tried first, who found
  what and lists the diff already shows, such as every test you added. The reviewer has the diff, and every extra line
  is one more to read before the part that matters.
- When more work changes a pull request, rewrite its description to describe the change as it now is, instead of
  adding to it. Appended rounds turn it into a history the reviewer has to replay to learn the result. A commit
  message or one short comment can say what changed since the last review.
