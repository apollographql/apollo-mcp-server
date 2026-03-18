---
default: minor
---

# Add Rhai HTTP module with `Http::get` and `Http::post`

You can now make HTTP requests from within your Rhai scripts using `Http::get` and `Http::post`. Both methods return a `Promise` that you can `.wait()` on to get the response.

Each method accepts an optional options map with support for `headers`, `body`, and `timeout`:

```rhai
// Simple GET request
let response = Http::get("https://api.example.com/data").wait();
print(response.status);
print(response.text());

// GET with custom headers and a timeout
let response = Http::get("https://api.example.com/data", #{
    headers: #{
        "Authorization": "Bearer my-token"
    },
    timeout: 30
}).wait();

let data = response.json();
```

```rhai
// POST with a JSON body
let response = Http::post("https://api.example.com/data", #{
    headers: #{
        "Content-Type": "application/json"
    },
    body: Json::stringify(#{
        key: "value"
    })
}).wait();

print(response.status);
```
