# http-server

A small HTTP service with a handful of routes, typed requests and responses, and a
JSON endpoint.

The whole router is one `match`, keyed on the **method and path together**:

```nym
func route(req: Request): Response = match (#(req.method, req.path)) {
  #(Method.Get, "/")       -> Response.text("Welcome 👋"),
  #(Method.Get, "/health") -> Response.json(#{ "status": "ok" }),
  #(Method.Post, "/echo")  -> Response.text(req.body),
  _                        -> Response.status(404).text("not found"),
}
```

What it shows:

- **A request/response model** — `Request` carries `method`, `path`, `body`, and
  query parameters; `Response` is built with `text`/`json`/`status` helpers.
- **`Method` as an enum** — matching `Method.Get`/`Method.Post` is exhaustive and
  typo-proof, unlike matching on strings.
- **`Option` for optional inputs** — `req.query("name") ?? "stranger"` supplies a
  default when a query parameter is missing.
- **Handlers are plain functions** — `serve` takes `route`, an ordinary
  `(Request) -> Response`, as its handler; no framework magic.

```sh
nymph run
# listening on http://127.0.0.1:8080

curl localhost:8080/hello?name=Ada
# Hello, Ada!
```

**Status:** 🚧 Aspirational — `std/http` and `std/json` don't exist yet. This sketch
is the API we want them to have: a synchronous `Server.bind(...).serve(handler)`
core, with `Request`/`Response` value types and JSON built on the stdlib's map/list
literals.
