# HTTP applet

The bundled `http` applet sends HTTP/1.1 requests. By default it writes one versioned `axe_http` JSON document with the request method and URL, final status and URL, headers, redirect history, resolved `Location`, and response body.

Inside Brush, run:

```bash
http https://example.org/
http -H 'Content-Type: application/json' -d '{"ready":true}' https://example.org/jobs
http --body --max-bytes 1048576 https://example.org/result
```

The method defaults to `GET`, or `POST` when a body is supplied. Use `-X/--method` to override it, `-H/--header` to add headers, and `--data-file PATH` to stream a request body (`-` reads stdin).

## Output and limits

The applet preserves body and header bytes. It encodes valid UTF-8 as `{"encoding":"utf8","data":"..."}` and other bytes as `{"encoding":"base64","data":"..."}`. `--body` writes raw body bytes instead of JSON.

The body limit defaults to 16 MiB; `--max-bytes` changes it. If the body exceeds the limit, the JSON keeps the response metadata, omits the body, and reports `limit_bytes` and `received_at_least_bytes`. With `--body`, you may get only the start of the body. Check the exit status before using it. `--timeout` covers the whole request, including redirects, and defaults to 30 seconds.

A complete HTTP response returns exit status `0`, even for `4xx` and `5xx`. Transport, TLS, redirect, body-limit, and output failures return `1`; invalid CLI arguments return `2`. The applet reports request failures in JSON by default and on stderr with `--body`. A failure to write JSON goes to stderr.

## Redirects and security

The applet does not follow redirects by default. Inspect `.response.resolved_location` one hop at a time. Use `-L/--follow` only if the entire possible chain is authorized. With `--follow`, the applet follows at most 10 redirects and omits `Authorization` and `Cookie` from redirected requests. It fails rather than replaying `POST`, `PUT`, `PATCH`, or `DELETE` across a `307` or `308`.

HTTPS trusts Mozilla roots and any additional CAs configured by the edition. `-k/--insecure` disables certificate and hostname verification. `--proxy` sets an explicit HTTP CONNECT proxy instead of proxy environment variables.

Passwords in URL userinfo are replaced by `[REDACTED]` only in structured URL fields. Raw headers, bodies, query parameters, and error messages are not scrubbed. Treat the whole document as sensitive.

## When to use another tool

Supported methods are `GET`, `HEAD`, `POST`, `PUT`, `DELETE`, `CONNECT`, `OPTIONS`, `TRACE`, and `PATCH`. The applet accepts HTTP/1.0 responses but does not send HTTP/1.0 requests.

Use Store `curl` for WebDAV/extension methods, HTTP/2, multipart, or preserving method and body across `307/308`. Use `ncat` for version-specific or malformed requests and raw protocol probes.
