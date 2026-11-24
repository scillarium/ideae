# Databases

A `.sql` file (or `.redis`) can run against MySQL, MariaDB, PostgreSQL or Redis.
A line over its text has the connection and the database; the results are under
the text, as wide as the file. `Shift+Esc` gives the file the whole window.

## Running

- `Ctrl+Enter`: the selection, else the statement at the cursor.
- `Ctrl+Shift+Enter`: the whole file, after saying how many statements it is.
- `Ctrl+F2`: stop. The rows read so far stay.
- `:name` in a statement is asked for before it runs, and goes in as a literal.
- Explain shows the plan of the statement at the cursor.
- `Ctrl+Alt+L` lays out the selection's statements, else the file's. Only spacing
  changes: a statement it could not lay out otherwise stays as written.
- An `UPDATE` or `DELETE` with no `WHERE` is asked about first. Everything else
  runs as written.

The editor completes tables, columns (by alias too), functions and keywords, and
after `JOIN` offers the whole condition from the foreign keys. A file with a database
chosen that has not connected yet reads the schema when you start typing: as kept from
before, else from the server.

Under the text: **Output** (what ran, how long, what failed), **Schema** (tables
and columns, their data and definition), **History**, and a tab for each result.
A pinned result stays when the next statement runs, and the new one opens after it.
It remembers its connection and database: once the file is on another, it can be
read, copied and exported as loaded, but nothing of it is run or saved there.

History keeps the latest 1000 statements in a file only you can read. It can be
cleared by statement, by connection or whole, and a removed connection takes its
history along. Statements that set a password run but are not kept.

## Scratch files

The Scratch menu makes `.sql` files of no project, on the file's connection, and lists
the latest. They are kept with the app's data, readable by you only, and stay out of
git. One can be renamed, moved into the project or deleted. A file closed with nothing
in it goes at once. One neither opened nor changed for 90 days, and in no task's tabs,
goes to the system's trash; where there is no trash it stays.

## Results

A result holds at most 10,000 rows; the arrows read the next ones, and a sort of a
result cut at that limit is asked of the server. Filter adds a `WHERE` on the
server, Search looks through the rows here, Columns puts some away, Row shows one
row a field a line. Export copies or saves CSV, TSV, JSON, Markdown or `INSERT`s;
saving a result cut at the limit runs a statement that only reads again and streams
all of its rows to the file.

A double click shows a value whole, JSON laid out on request. A value can be changed
where the server says which table and column it reads and that table's primary key
is in the result: not an expression, a view or a `UNION`. Changes wait, marked, until
Review shows their `UPDATE`s; Apply keeps all of them or none.

Tx: Manual keeps nothing until Commit. Closing a file asks first when a transaction
is open or changes wait. A connection that dropped while idle is made again before
the next statement; with a transaction open, that statement is not sent.

## Database panel

The Database panel (Alt+5) lists the connections, development ones apart from
production ones; the production heading says whether they are locked, and a click on
it locks or unlocks them. One opened lists its databases, a
database opened its tables and views, each read on opening on a connection of its
own; tables already read for a file's completions come from there. Refresh reads the
open ones again. A double click on a connection or a database opens a console, a
scratch file on it; on a table, one with a `SELECT` of its first 100 rows. Its title
has New connection and Import from DataGrip.

## Connections

Settings, Databases, or the connection's menu in a file. They are kept in
`connections.json` next to the app's state, readable by its owner, and never in a
project. Passwords are in the system's keyring, under `ideae: <connection>`: out of files,
backups and git, but any program of your user can ask for them while you are logged
in. Import from DataGrip brings data sources and their SSH settings, not their
passwords: DataGrip's own, and those added to a project in any JetBrains IDE (its
`.idea`), for the projects the IDEs opened lately and ideae's. **Look in a folder…**
adds the projects in a folder and under it, four levels down and 20,000 folders at
most, for older ones.

## Production

A connection's profile is **Development** or **Production**. Production is a safety
catch: nothing runs on such a connection until production is unlocked, which asks
first. Unlocked, every production connection runs for 10 minutes (5, 10, 15 or 30, in
Settings, Databases), then they lock again by themselves; the title bar says until
when, and a click locks at once; time asleep counts. Unlocking takes a click on the
button, not Enter. Locked, nothing reaches the server but a Rollback: a statement, a
Commit, saved cells, an export or an import is refused by the session itself, which
stays connected with its transaction, and asks again; another database chosen is
taken once unlocked, and the list of databases, a table's definition, the schema and
the panel read nothing new. A locked production file connects only when something
runs. A production connection is changed, made a development one, overwritten by an
import or removed only while unlocked.

It keeps a statement from going to production by mistake, yours or of a file left
open; an agent has no way to run a statement through the app or to answer the
question. It is no secret: the connection's password is in the keyring like any other.

A connection is direct, or goes through a **bastion**, set up once and chosen for
as many connections as go through it. A bastion is reached with your own `ssh`: its
keys, agent, `known_hosts` and `~/.ssh/config` apply. A key with a passphrase has to
be in `ssh-agent`, and a host key has to be known: connect once in a terminal.

Encryption: **Prefer** (the default) is encrypted when the server can do it,
**Require** always, without checking the certificate, **Verify** also checks it,
against a CA file or the public authorities, **Off** never. Through a tunnel, Verify
checks the name of the database for PostgreSQL, only the signer for MySQL, and is
not available for Redis.
