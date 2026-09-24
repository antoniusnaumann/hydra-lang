// channels.hy — trails handing values to their siblings, with no channel
// declared anywhere: the block is the channel set (spec/hydra_channels.md).
//
//	hydra run examples/channels.hy

// A handoff. `receive` answers with the value and the trail that sent it; the
// second may be ignored, and usually is (§1, §6.2).
parallel
	_ = send("ready") || msg, ch := receive()
	                  || print("got \(msg) from trail \(ch)")
end

// One producer, one worker, no queue and no pool object. `send` with no index
// goes to whoever asks first, and the worker leaves its loop when the channel
// comes back `.null` — the one answer a producer cannot fake (§4).
total := 0
parallel
	for job in [1, 2, 3] || while alive()
	_ = send(job)        || work, ch := receive()
	end                  || if ch == .null
	                     || break
	                     || end
	                     || total = total + work
	                     || end
end
print("the worker ran \(total)")

// `.broadcast` buffers one copy for every eligible trail; `.detach` buffers one
// and does not wait. Both answer `.false` only when every eligible trail has
// already ended (§5).
parallel
	_ = send(.config, mode = .broadcast) || print("a took \(receive())") || print("b took \(receive())")
end

// An index addresses one sibling, and `channel()` says where "here" is, which
// is what makes a pipeline writable (§3, §6.3).
parallel
	_ = send("work", 1) || job, from := receive()
	                    || print("trail \(channel()) took \(job) from \(from)")
end

// A losing trail parked in `receive` is woken with the closed answer and then
// runs no further statement — cancellation is value-level, and this is the one
// place "never interrupted" bends (§6.5).
race
	done := .true || v, ch := receive()
	              || print("this never runs")
end
print("the race is over")

// The variadics underneath: `name*` collects, and everything after it can only
// be named (§6.1).
fn log(prefix, values*, sep = " ")
	print("\(prefix)\(sep)\(len(values)) value(s)")
end

log("none")
log("two", 1, 2)
log("two", 1, 2, sep = ": ")

// A bare `*` closes the positional list and collects nothing.
fn retry(host, *, attempts = 3)
	print("\(host) x\(attempts)")
end

retry("eu")
retry("eu", attempts = 5)

// Any function may answer with several values. By convention the first is the
// meaningful one and the rest are additional information, so dropping them is
// always safe (§6.2).
fn measure(text)
	return text, len(text)
end

only := measure("hydra")
value, size := measure("hydra")
print("\(only) is \(size) long, and \(value) is the same thing")
