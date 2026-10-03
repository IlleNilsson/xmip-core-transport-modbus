# xmip-core-transport-modbus

Modbus TCP transport: one MBAP-framed request or response is one Stream, the function code and unit id kept beside it. A technology of [xmip-core-transport](https://github.com/IlleNilsson/xmip-core-transport).

A Receive Location keeps its listener, bound on the first receive, and the clients' connections open on it between their requests (`transport::serving::Serving`): each receive takes the next request from whichever client sends first. Until 2026-09-27 each receive bound a listener of its own and a peer between receives was refused; until 2026-10-02 a receive read one client's requests until it closed.

## Acknowledgement

The client is answered after the whole receive cycle: it waits on its connection for the response to its request. Accepted answers the empty response. Refused answers an exception the client does not send again (MODBUS Application Protocol Specification V1.1b3, section 7): *illegal function* (01) for a sender not identified or not permitted, *illegal data value* (03) for content refused. Failed answers the exception *server device busy* (06), which tells the client to send the request again. A request let go without a verdict shuts its connection (`transport::answer::Answer`), so the client is not left waiting and sends it again. A Send Location fails a send the server answers with an exception: retryable on *acknowledge* (05) or *server device busy* (06), permanent otherwise. Each request arrives whole.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
