// Run: hydra run examples/cli_greet.hy -- Hydra -n 2
// Help: hydra run examples/cli_greet.hy -- --help
use cli
use list

parser := cli::parser(description = "A small greeting tool")
cli::add_argument(&parser, "name", help = "Who to greet")
cli::add_argument(&parser, "-n", "--count", type = :int, default = 1,
choices = [1, 2, 3], help = "Number of greetings")
cli::add_argument(&parser, "-v", "--verbose", action = :count,
help = "Increase verbosity; repeat as -vv")

// Bad input prints an error and exits 2; help prints and exits 0.
args := cli::parse_args(parser)
for index in list::range(args.count)
	print "Hello, \(args.name)!"
end

if args.verbose > 0
	print "Printed \(args.count) greetings"
end
