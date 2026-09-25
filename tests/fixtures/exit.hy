// Constructing or returning an exit list does not invoke the handler.
fn request_exit(code)
	return [:exit, code]
end
pending := request_exit(7)
print("before exit")
// Leaving it unconsumed invokes the default handler and exits with status 7.
pending
print("unreachable")
