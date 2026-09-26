// Run: hydra run examples/fetch_url.hy -- https://example.com
use env
use http

args := env::args()
url := get(args, 0, "https://example.com")
response, reason := http::get url, :null, timeout = 10
if reason != :null
	panic "Request failed: \(reason)"
end

// HTTP status codes remain data, including 4xx and 5xx.
print response.status
print response.body
