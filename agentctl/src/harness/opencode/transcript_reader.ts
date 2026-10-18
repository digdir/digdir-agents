// Prints one OpenCode session from its database as JSON lines, `{"seq", "type", "data"}` per
// message in order, and only the last bytes of them that fit the budget, as `tail -c` would.
// Runs as Bun inside the OpenCode binary. It is single-quoted into a shell script, so it must not
// contain a single quote.
import { Database } from "bun:sqlite";

const [database, session, budget] = process.argv.slice(1);
const limit = Number(budget);
const rows = new Database(database, { readonly: true })
  .query("SELECT seq, type, data FROM session_message WHERE session_id = ? ORDER BY seq")
  .all(session) as { seq: number; type: string; data: string }[];
// Encode from the newest message back, and only as many as the budget can show.
const lines: Buffer[] = [];
let size = 0;
for (let index = rows.length - 1; index >= 0 && size < limit; index -= 1) {
  const { seq, type, data } = rows[index];
  const line = Buffer.from(JSON.stringify({ seq, type, data: JSON.parse(data) }) + "\n");
  lines.push(line);
  size += line.length;
}
const output = Buffer.concat(lines.reverse());
process.stdout.write(output.subarray(Math.max(0, output.length - limit)));
