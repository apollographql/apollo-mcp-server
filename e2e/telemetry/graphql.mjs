import { createServer } from 'node:http';

// Answers every operation with the same data, and logs the `traceparent` each
// request carried so the run can check propagation to the GraphQL endpoint.
const server = createServer(async (request, response) => {
  if (request.method === 'GET' && request.url === '/health') {
    response.writeHead(200).end('ready');
    return;
  }
  for await (const _ of request);
  console.log(JSON.stringify({ traceparent: request.headers.traceparent ?? null }));
  response
    .writeHead(200, { 'content-type': 'application/json' })
    .end(JSON.stringify({ data: { hello: 'world' } }));
});
server.listen(4201, '127.0.0.1', () => console.log('GraphQL fixture listening on 4201'));
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => server.close());
}
