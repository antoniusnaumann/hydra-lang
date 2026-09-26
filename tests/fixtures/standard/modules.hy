// Text and JSON compose without extra conversion machinery.
use text
use json
use env
use time

words := text::split "  hello   Hydra  "
record := { :message : text::join(words, " "), :ready : :true }
encoded := json::stringify record
restored := json::parse encoded
print restored.message
print restored.ready

// Fallback readers explain failure in their second return value.
value, reason := json::parse "not JSON", :null
print reason
print time::format(0, "%Y-%m-%d")

// Missing environment settings can have useful defaults.
name := env::get "HYDRA_EXAMPLE_NAME_349718750", "friend"
print name
