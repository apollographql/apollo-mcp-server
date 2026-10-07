import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { McpError } from '@modelcontextprotocol/sdk/types.js';

// Exercise Apollo's supported resource URI and routing through a real SDK
// lifecycle. These checks supplement, but do not alter, the upstream score.
export async function verifyApolloContent(artifacts) {
  const client = new Client({ name: 'apollo-conformance-supplement', version: '1.0.0' });
  const transport = new StreamableHTTPClientTransport(
    new URL('http://127.0.0.1:4100/mcp?app=conformance&appTarget=mcp'),
    { fetch: (url, init) => fetch(url, {
      ...init,
      signal: init?.signal
        ? AbortSignal.any([init.signal, AbortSignal.timeout(10_000)])
        : AbortSignal.timeout(10_000),
    }) },
  );
  const evidence = {};
  let failure;
  try {
    await client.connect(transport, { timeout: 10_000 });
    assert.equal(transport.protocolVersion, '2025-11-25');

    evidence.resources = await client.listResources();
    const uri = 'ui://widget/conformance#local-fixture-v1';
    assert.equal(evidence.resources.resources.length, 1);
    assert.equal(evidence.resources.resources[0].uri, uri);
    assert.equal(evidence.resources.resources[0].mimeType, 'text/html;profile=mcp-app');

    evidence.resource = await client.readResource({ uri });
    assert.equal(evidence.resource.contents.length, 1);
    const content = evidence.resource.contents[0];
    assert.equal(content.uri, uri);
    assert.equal(content.mimeType, 'text/html;profile=mcp-app');
    assert.equal(content.text, await readFile(new URL('./apps/conformance/index.html', import.meta.url), 'utf8'));

    await assert.rejects(
      client.readResource({ uri: 'ui://widget/missing' }),
      (error) => {
        assert(error instanceof McpError, 'Expected an MCP error, not a transport failure');
        assert.equal(error.code, -32002, 'Expected resource-not-found for 2025-11-25');
        evidence.missingResource = { code: error.code, message: error.message };
        return true;
      },
    );

    evidence.prompt = await client.getPrompt({ name: 'test_simple_prompt' });
    assert.deepEqual(evidence.prompt.messages, [{
      role: 'user', content: { type: 'text', text: 'This is a simple prompt for testing.' },
    }]);

    await transport.terminateSession();
    evidence.status = 'SUCCESS';
    console.log('Verified Apollo resource listing, exact HTML content, missing-resource error, and simple prompt content.');
  } catch (error) {
    evidence.status = 'FAILURE';
    evidence.error = error.stack ?? String(error);
    failure = error;
  }
  try {
    await client.close();
  } catch (error) {
    console.error('Failed to close supplemental MCP client:', error);
    evidence.status = 'FAILURE';
    evidence.error ??= error.stack ?? String(error);
    failure ??= error;
  }
  try {
    await writeFile(join(artifacts, 'apollo-content.json'), `${JSON.stringify(evidence, null, 2)}\n`);
  } catch (error) {
    console.error('Failed to write supplemental MCP evidence:', error);
    failure ??= error;
  }
  if (failure) throw failure;
}
