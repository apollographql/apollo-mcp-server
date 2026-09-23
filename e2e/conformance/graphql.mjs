import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { buildSchema, graphql } from 'graphql';

const schema = buildSchema(await readFile(new URL('./schema.graphql', import.meta.url), 'utf8'));
const rootValue = {
  text: () => 'This is a simple text response for testing.',
  intentionalError: () => {
    throw new Error('This tool intentionally returns an error for testing');
  },
};

const server = createServer(async (request, response) => {
  if (request.method === 'GET' && request.url === '/health') {
    response.writeHead(200).end('ready');
    return;
  }
  if (request.method !== 'POST' || request.url !== '/graphql') {
    response.writeHead(404).end();
    return;
  }
  try {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const { query, variables, operationName } = JSON.parse(Buffer.concat(chunks).toString());
    const result = await graphql({ schema, source: query, rootValue, variableValues: variables, operationName });
    console.log(JSON.stringify({ operationName, result }));
    response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(result));
  } catch (error) {
    console.error(error);
    response.writeHead(400).end();
  }
});
server.listen(4101, '127.0.0.1', () => console.log('GraphQL fixture listening on 4101'));
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => server.close());
}
