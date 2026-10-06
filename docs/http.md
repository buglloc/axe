# HTTP applet

The bundled `http` applet makes bounded HTTP/1.1 requests. By default it writes one versioned `axe_http` JSON document containing the request method and URL, final status and URL, headers, redirect history, resolved `Location`, and response body.

Inside Brush, run:

```bash
http https://example.org/
http -H 'Content-Type: application/json' -d '{"ready":true}' https://example.org/jobs
http --body --max-bytes 1048576 https://example.org/result
```

The method defaults to `GET`, or `POST` when a body is supplied. Use `-X/--method` to override it, `-H/--header` to add headers, and `--data-file PATH` to stream a request body (`-` reads stdin).

## Output and limits

Body and header values preserve bytes: valid UTF-8 uses `{"encoding":"utf8","data":"..."}`; other bytes use `{"encoding":"base64","data":"..."}`. `--body` writes raw body bytes instead of JSON.

The body limit defaults to 16 MiB; `--max-bytes` changes it. On overflow, JSON output retains response metadata but omits the body and reports `limit_bytes` and `received_at_least_bytes`. Raw-body output may contain a partial prefix; check the exit status before using it. `--timeout` bounds the whole request, including redirects, and defaults to 30 seconds.

A completed response exits with status `0`, including HTTP `4xx` and `5xx`. Transport, TLS, redirect, body-limit, and output failures exit with status `1`; invalid CLI arguments exit with status `2`. Request failures use JSON in the default mode and stderr in `--body` mode. Failure to write JSON is reported on stderr.

## Redirects and security

Redirects are not followed by default. Inspect `.response.resolved_location` one hop at a time; use `-L/--follow` only when the entire possible chain is authorized. Following is limited to 10 redirects. Redirected requests do not forward `Authorization` or `Cookie`. `POST`, `PUT`, `PATCH`, and `DELETE` cannot be replayed across `307/308`; the request fails rather than preserving the method and body.

HTTPS trusts Mozilla roots and any additional CAs configured by the edition. `-k/--insecure` disables certificate and hostname verification. `--proxy` sets an explicit HTTP CONNECT proxy instead of proxy environment variables.

Passwords in URL userinfo are replaced by `[REDACTED]` only in structured URL fields. Raw headers, bodies, query parameters, and error messages are not scrubbed. Treat the whole document as sensitive.

## When to use another tool

Supported methods are `GET`, `HEAD`, `POST`, `PUT`, `DELETE`, `CONNECT`, `OPTIONS`, `TRACE`, and `PATCH`. The applet accepts HTTP/1.0 responses but does not send HTTP/1.0 requests.

Use Store `curl` for WebDAV/extension methods, HTTP/2, multipart, or preserving method and body across `307/308`. Use `ncat` for version-specific or malformed requests and raw protocol probes.
