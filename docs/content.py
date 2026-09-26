"""Short reference copy. Function signatures are read from src/value.rs."""

# name: (section, result, description)
BUILTINS = {
    "print": ("Values", ":null", 'Write one value as text, followed by `terminator`. Use `""` to omit the newline.'),
    "len": ("Values", "number", "Count list elements, dict entries, or Unicode characters in a string."),
    "has": ("Values", "boolean", "Test for a dict key or list index. Negative indices count from the end."),
    "get": ("Values", "value", "Read a dict key or list index, returning `fallback` when absent. Does not insert anything."),
    "push": ("Values", "number", "Append a copied value to a list; return its new length. The list argument must use `&`."),
    "return": ("Control", "[:return, …values]", "Construct a return request. Unconsumed, return its payload from the current function; an empty request returns `:null`."),
    "break": ("Control", ":break", "Construct a loop-stop request. Unconsumed, stop the nearest loop in this function; a parallel loop stops as a whole."),
    "continue": ("Control", ":continue", "Construct an iteration-stop request. Unconsumed, skip the rest of this iteration; a parallel iteration ends its trail."),
    "reject": ("Control", "[:reject, msg]", "Construct a rejection. Unconsumed in a function, try its next overload. Unhandled exhaustion reports all refusal messages and fails."),
    "exit": ("Control", "[:exit, code]", "Construct an exit request. Unconsumed, stop the program with an integer status from 0 through 255."),
    "panic": ("Control", "[:panic, msg]", "Construct a panic request. Unconsumed, stop the program with a rendered message, call trace, and status 1."),
    "alive": ("Concurrency", "boolean", "Whether the current trail is still live. Returns `:true` outside a trail."),
    "channel": ("Concurrency", "number", "The current trail’s zero-based index in its block. Requires a trail."),
    "send": ("Concurrency", "boolean", "Send to sibling trails; omit destinations for any sibling. Default `:wait` waits for receipt; `:detach` queues one message; `:broadcast` queues a copy per eligible sibling. False if none remain. Requires a trail."),
    "receive": ("Concurrency", "value, channel", "Wait for a sibling’s message and its sender index. Omit sources for any sibling. Returns `:null, :null` once none can send. Requires a trail."),
}

FS = {
    "join": ("Paths", "string", 'Join and normalize path components. `join("a", "/b")` yields `"a/b"`. Removes `.`; preserves `..`.'),
    "parent": ("Paths", "string", 'Parent directory; `"."` if the path has no directory component.'),
    "name": ("Paths", "string", "Final path component, including its extension."),
    "stem": ("Paths", "string", "Final component without its last extension."),
    "extension": ("Paths", "string", 'Last extension without its dot; `""` if absent.'),
    "absolute": ("Paths", "string", "Resolve against the working directory. Does not require existence or resolve symlinks."),
    "cwd": ("Locations", "string", "Current working directory."),
    "home": ("Locations", "string", "Current user’s home directory. Crashes if unavailable."),
    "temp": ("Locations", "string", "System temporary directory. Does not create a file or directory."),
    "exists": ("Inspect", "boolean", "Whether the path exists and can be inspected. Follows symlinks."),
    "is_file": ("Inspect", "boolean", "Whether the path is a regular file. False if absent or inaccessible."),
    "is_dir": ("Inspect", "boolean", "Whether the path is a directory. False if absent or inaccessible."),
    "size": ("Inspect", "bytes, reason", "Size from filesystem metadata, in bytes. Without a fallback, errors crash."),
    "modified": ("Inspect", "seconds, reason", "Modification time as Unix seconds, including fractions. Without a fallback, errors crash."),
    "read": ("Read", "string, reason", "Read the whole file as UTF-8. Without a fallback, read and encoding errors crash."),
    "lines": ("Read", "list, reason", "Read UTF-8 lines, without line endings. Without a fallback, errors crash."),
    "list": ("Read", "list, reason", "List matching entries as paths joined to `dir`. Sort within each directory; recurse depth-first when requested. Patterns support `*` and `?`. Without a fallback, errors crash."),
    "write": ("Write", "path, bytes", "Write text; return the path and UTF-8 byte count. `:replace` truncates, `:append` appends, `:new` requires a new file. Creates parent directories by default."),
    "copy": ("Write", "target", "Copy a file or directory tree. Overwrite and create parent directories by default. Other failures crash."),
    "move": ("Write", "target", "Rename a file or directory. Create parents by default; existing targets require `overwrite = :true`. Cross-filesystem moves may fail."),
    "remove": ("Write", "boolean", "Remove a file or empty directory; `recursive = :true` removes a directory tree. False if absent; other failures crash."),
    "make_dir": ("Write", "path", "Create a directory and, by default, its parents. An existing directory is fine; other failures crash."),
}

CONTROL_ROWS = [
    ("break()", ":break", "Nearest loop in this function", "Stop the loop; for a parallel loop, cancel its other iterations."),
    ("continue()", ":continue", "Nearest loop in this function", "End the current iteration."),
    ("return(values*)", "[:return, …values]", "Current function", "Return the payload; no payload means `:null`."),
    ("reject(msg)", "[:reject, msg]", "Function / overload resolution", "Try the next candidate. Unhandled exhaustion reports every refusal and fails."),
    ("exit(code)", "[:exit, code]", "Program", "Stop with status 0–255."),
    ("panic(msg)", "[:panic, msg]", "Program", "Stop with a diagnostic and status 1."),
]

ATOM_GROUPS = [
    ("values", "Values", [
        (":true", "Boolean true. Also returned by successful predicates."),
        (":false", "Boolean false; falsy."),
        (":null", "Absence or an empty result; falsy. Everything else is truthy, including 0 and empty collections."),
    ]),
    ("send-modes", "Send modes", [
        (":wait", "Wait until a sibling receives the message. Default for `send`."),
        (":detach", "Queue for one eligible sibling and continue immediately."),
        (":broadcast", "Queue one copy for each eligible sibling already spawned."),
    ]),
    ("write-modes", "Write modes", [
        (":replace", "Create or truncate a file. Default for `fs::write`."),
        (":append", "Append; create the file if absent."),
        (":new", "Create a new file; fail if it already exists."),
    ]),
    ("fs-reasons", "Filesystem reasons", [
        (":not_found", "Path does not exist."),
        (":denied", "Permission denied."),
        (":exists", "Path already exists."),
        (":is_dir", "A file operation encountered a directory."),
        (":not_dir", "A directory operation encountered a non-directory."),
        (":encoding", "Invalid data, including non-UTF-8 text."),
        (":io", "Other I/O failure."),
    ]),
]

LANGUAGE = [
    ("bindings", "Bindings & values", "`:=` declares; `=` assigns; `_ = expression` explicitly discards a result. Lists and dicts copy by value. Use `&place` to pass a reference; mutations through it reach the caller.", 'name := "Hydra"\nuser := { :name : name, :ready : :true }\nprint user.name  // user[:name]\nitems := [1, 2]\npush &items, 3'),
    ("calls", "Calls", "Omit parentheses only for an outer statement or assignment RHS. Arguments use commas; nested calls require parentheses. A first argument starts on the callee’s line. Bare function names are values; zero-argument calls use `()`.", 'fn add(a, b)\n\treturn a + b\nend\n\ntotal := add 20, len([1, 2])\nprint total'),
    ("spacing", "Significant spacing", "`f -1` calls with a negative argument; `f - 1` subtracts. `f [1]` calls with a list; `f[1]` indexes. `f &x` passes a reference; `f & x` is bitwise AND. The formatter preserves these distinctions.", None),
    ("continuation", "Continuation", "A single newline continues an expression whenever syntax permits. A blank or whitespace-only line stops it. Comment-only lines are transparent. Precedence stays unchanged.", 'foo\n.bar\n.baz  // foo.bar.baz\n\n32\n-a    // 32 - a\n\n32\n\n-a    // two statements'),
    ("functions", "Functions & results", "Blocks close with `end`; indentation is cosmetic. Parameters can have defaults, require a reference (`&x`), or collect positional arguments (`xs*`). Parameters after `*` or `xs*` are named-only. `return` is an ordinary builtin; a bare `return` calls it with no arguments.", 'fn pair(x, offset = 1)\n\treturn x, x + offset\nend\n\na, b := pair 4\nf := pair  // keep the function itself'),
    ("handlers", "Control values", "A control builtin constructs an atom or tagged list. Binding, passing, or returning that value consumes it. An expression statement leaves it to a handler. Loop and return handlers stay within the current function; forward a request explicitly to let the caller handle it.", 'fn return_if(condition, value)\n\tif condition\n\t\treturn return(value)\n\tend\nend'),
    ("overloads", "Overloads & rejection", "Calls try candidates by signature; `:reject` or `[:reject, msg]` tries the next candidate. A successful fallback is silent. If all reject, an unconsumed rejection reports all messages and fails. Builtins can be shadowed; `::name` explicitly selects a global builtin.", None),
    ("modules", "Modules", "`use fs` imports a module; `fs::read(...)` selects a function. `use fs as files` renames it; `use fs as *` imports its names. Standard modules: `fs`, `env`, `text`, `json`, `io`, `time`, and `http`.", 'use fs\ntext, reason := fs::read "optional.txt", ""\nif reason != :null\n\tprint "Using the default text"\nend'),
    ("loops", "Branches & loops", "Use `if` / `else if` / `else`, `for item in items`, or `while condition`. `break()` stops the nearest loop; `continue()` skips an iteration. Neither crosses a function boundary. There are no labeled breaks.", 'for n in [1, 2, 3]\n\tif n == 2\n\t\tcontinue()\n\tend\n\n\tprint n\nend'),
    ("trails", "Parallel trails", "Each `||` column is an independent trail; rows do not synchronize. `parallel` waits for all trails; `race` cancels losers after a winner finishes. `send` and `receive` communicate within the block. Parallel loops support the same loop-control atoms.", 'parallel\n\tsend "ready" || message := receive()\n\t             || print message\nend'),
    ("atoms", "Atoms & strings", "Atoms use `:name` or `:\"quoted name\"`. Bare names consume punctuation until whitespace, a delimiter (`()[]{},:\"`), or `//` / `||`. A dict keeps the colon between key and value. Strings are double-quoted; `\\(expression)` interpolates a value.", 'status := :some-other-prop+info\nrecord := { :status : status }\nprint "Status: \\(record.status)"'),
]

# module: (summary, function descriptions, short usage note, example)
MODULES = {
    "fs": ("Filesystem paths, inspection, reading, and writing.", FS,
        "Readers return value, reason. Without a fallback, operational failures crash.", None),
    "env": ("Script arguments, environment variables, and the host platform.", {
        "args": ("Environment", "list", "Arguments after `--`, excluding the script name. Embedding hosts supply these in VM options."),
        "get": ("Environment", "string, reason", "Read a variable. Missing names return `fallback, :not_found`; non-Unicode values return `fallback, :encoding`. Without a fallback, failures crash. Invalid names always crash."),
        "all": ("Environment", "dict", "Snapshot Unicode environment variables, sorted by name. Keys are atoms; variables with non-Unicode names or values are omitted."),
        "platform": ("Environment", "atom", "Host OS, such as `:macos`, `:linux`, or `:windows`."),
    }, "Run `hydra run script.hy -- arg1 arg2`. Interpreter options go before `--`. This module does not mutate the process environment.",
        'use env\nname := env::get "USER", "friend"\nprint "Hello, \\(name)"'),
    "text": ("Unicode string operations. Positions count characters, not bytes.", {
        "split": ("Strings", "list", "With `:null`, split on Unicode whitespace and discard empty fields. An explicit separator preserves empty fields; an empty separator splits into characters."),
        "join": ("Strings", "string", "Join a list of strings with the separator. An empty list produces an empty string."),
        "trim": ("Strings", "string", "Remove leading and trailing Unicode whitespace."),
        "replace": ("Strings", "string", "Replace every non-overlapping literal match. An empty `from` inserts at character boundaries, including both ends."),
        "find": ("Strings", "number or :null", "Zero-based character position of the first literal match; `:null` if absent. An empty needle matches at zero."),
        "starts_with": ("Strings", "boolean", "Whether text begins with the literal prefix."),
        "ends_with": ("Strings", "boolean", "Whether text ends with the literal suffix."),
        "lower": ("Strings", "string", "Unicode lowercase conversion, independent of locale."),
        "upper": ("Strings", "string", "Unicode uppercase conversion, independent of locale. May change character count."),
    }, "These functions require strings; they do not silently render other values.",
        'use text\nwords := text::split "  hello   Hydra  "\nprint text::join(words, " ")'),
    "json": ("Convert between JSON text and Hydra values.", {
        "parse": ("Conversion", "value, reason", "Objects become dicts with atom keys; arrays become lists; booleans and null become `:true`, `:false`, and `:null`. Invalid input gives `fallback, :invalid`, or crashes without a fallback."),
        "stringify": ("Conversion", "string", "Encode values, preserving dict order. Other atoms become JSON strings. `pretty = :true` indents. Functions, non-finite numbers, cycles, and excessive nesting crash."),
    }, "Numbers use Hydra’s floating-point representation: large JSON integers can lose precision. Object keys become atoms, but ordinary JSON string values stay strings.",
        'use json\nencoded := json::stringify { :name : "Hydra", :ready : :true }\nrecord := json::parse encoded\nprint record.name'),
    "io": ("UTF-8 standard input, output, and error streams.", {
        "read": ("Input", "string, reason", "Read stdin to EOF, preserving line endings. Empty input produces an empty string."),
        "read_line": ("Input", "string or :null, reason", "Read one line, removing its LF or CRLF ending. EOF produces `:null`; an empty line produces an empty string."),
        "lines": ("Input", "list, reason", "Read stdin to EOF as a list of lines, without line endings. This collects the entire input."),
        "write": ("Output", "bytes", "Render one value without a newline to `:stdout` or `:stderr`, flush, and return its UTF-8 byte count. Failures crash."),
        "flush": ("Output", ":null", "Flush `:stdout` or `:stderr`. Failures crash."),
        "is_terminal": ("Output", "boolean", "Whether `:stdin`, `:stdout`, or `:stderr` is connected to a terminal."),
    }, "Input supports `:stdin` only. Readers return value, reason; fallback overloads report `:encoding` or `:io`, while strict overloads crash. Read one line at a time for bounded memory. Concurrent readers share and serialize access to stdin.",
        'use io\nline := io::read_line()\nwhile line != :null\n\tprint line\n\tline = io::read_line()\nend'),
    "time": ("Clocks, sleeping, and timestamp conversion. All durations use seconds.", {
        "now": ("Clocks", "seconds", "Unix timestamp with fractional seconds. The system clock can jump."),
        "monotonic": ("Clocks", "seconds", "Elapsed seconds from the host process’s first call. Use differences to measure durations; unrelated to calendar time."),
        "sleep": ("Clocks", ":null", "Wait a finite, nonnegative number of seconds while other trails run. Scheduling can make the actual wait longer."),
        "parse": ("Dates", "seconds, reason", "Parse an RFC 3339 timestamp with an explicit offset or Z. Return Unix seconds; invalid input gives `fallback, :invalid`, or crashes without a fallback."),
        "format": ("Dates", "string", "Format Unix seconds in UTC using strftime directives such as `%Y-%m-%d %H:%M:%S`. Default `%+` produces RFC 3339. Invalid formats or dates crash."),
    }, "Use `now` for timestamps and `monotonic` for elapsed time. Negative timestamps represent dates before 1970.",
        'use time\nstart := time::monotonic()\ntime::sleep 0.01\nprint time::monotonic() - start'),
    "http": ("HTTP and HTTPS requests, text responses, and binary downloads.", {
        "request": ("Requests", "response, reason", "Send a method string or atom, with an optional UTF-8 body. Response fields are `.status`, `.headers`, `.body`, and final `.url`. HTTP error statuses remain ordinary responses."),
        "get": ("Requests", "response, reason", "GET shorthand for `request`. Response body must be UTF-8."),
        "post": ("Requests", "response, reason", "POST a UTF-8 string. Set content-type explicitly; use `json::stringify` for JSON payloads."),
        "head": ("Requests", "response, reason", "HEAD shorthand. Return status and headers with an empty `.body`, even when content-length describes a nonempty resource."),
        "put": ("Requests", "response, reason", "PUT a UTF-8 string. Set content-type explicitly; use `json::stringify` for JSON payloads."),
        "patch": ("Requests", "response, reason", "PATCH with a UTF-8 string. Set the content-type required by the server’s patch format."),
        "delete": ("Requests", "response, reason", "DELETE shorthand, with an optional named `body`."),
        "options": ("Requests", "response, reason", "OPTIONS shorthand, with an optional named `body`. Inspect response headers for supported operations."),
        "connect": ("Requests", "response, reason", "Send CONNECT using the URL’s authority as the request target. Return the handshake response and close the connection; tunnel streams are not exposed."),
        "trace": ("Requests", "response, reason", "TRACE shorthand. Return the server’s diagnostic response."),
        "download": ("Requests", "path, reason", "GET binary data to a file. Require a 2xx status, create parent directories, and replace the target only after the complete bounded download succeeds. Failure leaves an existing target intact."),
    }, "Method shorthands share `request` options and fallback behavior. Use `request` for custom methods. Headers are an atom-keyed dict of strings or lists of strings. Response header names are lowercase, with lists preserving repeated values. TLS certificates are verified; redirects are followed up to 10. `timeout` is a positive total duration in seconds; `max_bytes` caps the decoded body (16 MiB by default, buffered in memory). Each call has a strict and a fallback overload. Operational failures report a reason or crash; invalid arguments always crash.",
        'use http\nuse json\nbody := json::stringify { :ready : :true }\nresponse, reason := http::patch "https://example.com/item", body, :null,\n\theaders = { :content-type : "application/json" }\nif reason == :null\n\tprint response.status\nelse\n\tprint reason\nend'),
}

ATOM_GROUPS += [
    ("streams", "Standard streams", [
        (":stdin", "Input stream for `io` readers and `is_terminal`."),
        (":stdout", "Default stream for `io::write`, `flush`, and `is_terminal`."),
        (":stderr", "Error output stream for `io::write`, `flush`, and `is_terminal`."),
    ]),
    ("module-reasons", "Additional module reasons", [
        (":invalid", "Malformed JSON or timestamp."),
        (":timeout", "HTTP operation exceeded its time limit."),
        (":dns", "HTTP hostname could not be resolved."),
        (":tls", "TLS negotiation or certificate verification failed."),
        (":network", "Other HTTP connection or protocol failure."),
        (":redirect", "HTTP redirect limit or redirect failure."),
        (":too_large", "HTTP response exceeded `max_bytes`."),
        (":http_status", "A download received a non-2xx response. Other HTTP functions return the response normally."),
    ]),
]
