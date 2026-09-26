# HTTP applet

The bundled `http` applet makes bounded HTTP requests. By default, `http` emits one versioned `axe_http` JSON document with the request method and URL, final status and URL, resolved `Location`, redirect history, headers, and a bounded response body. UTF-8 body and header values use `{"encoding":"utf8","data":"..."}`; other bytes use Base64. A completed bounded response exits with status `0`, even for HTTP `4xx` or `5xx`. Body-limit and redirect failures can occur after response headers; they return a JSON error with status `1`. Passwords in userinfo are replaced by `[REDACTED]` only in structured URL fields. Raw headers and query parameters remain verbatim: treat the entire evidence document as sensitive.

Inside Brush, run:

```bash
http https://example.org/
http -H 'Content-Type: application/json' -d '{"ready":true}' https://example.org/jobs
http --body --max-bytes 1048576 https://example.org/result
```

The response-body limit defaults to 16 MiB; change it with `--max-bytes`. On overflow, the JSON error retains status, URL, version, headers, and redirect history but omits the body and reports `limit_bytes` and `received_at_least_bytes`. `--body` writes the raw body to stdout, which may contain a valid partial prefix on overflow.

Redirects are not followed unless you specify `-L/--follow`. This preserves the original response and prevents requests from silently leaving their intended scope. Inspect `.response.resolved_location` one hop at a time; use `-L` only if the entire possible chain is authorized. Redirected requests do not forward `Authorization` or `Cookie`. The backend does not preserve `POST`, `PUT`, `PATCH`, or `DELETE` across `307/308`. `--data-file -` reads the request body from stdin, `--proxy` specifies an HTTP CONNECT proxy, and `--timeout` bounds the whole request. HTTPS trusts Mozilla roots and any additional CAs configured by the edition.

The HTTP/1.1 request backend supports `GET`, `HEAD`, `POST`, `PUT`, `DELETE`, `CONNECT`, `OPTIONS`, `TRACE`, and `PATCH`. It accepts HTTP/1.0 responses but does not select HTTP/1.0 for requests. Use Store `curl` for WebDAV/extension methods, HTTP/2, preserving method/body across `307/308`, or multipart; use `ncat` for version-specific or malformed requests, request smuggling, and raw protocol probes.
