// Minimal WS/JSON-RPC client to live-validate the harness backend end-to-end.
// Usage: node client.js "ws://127.0.0.1:8777" "your prompt" [thinking:true|false]
const WebSocket = require('ws');

const url = process.argv[2] || 'ws://127.0.0.1:8777';
const content = process.argv[3] || 'Say hello in one short sentence.';
const enableThinking = (process.argv[4] || 'false') === 'true';

const ws = new WebSocket(url);
let nextId = 1;
let replyText = '';
let tokenCount = 0;
const timeout = setTimeout(() => { console.error('TIMEOUT after 180s'); process.exit(2); }, 180000);

function send(method, params) {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return id;
}

ws.on('open', () => {
  console.log('[open]', url);
  // 1) tokenize
  send('tokenize', { text: 'Hello world' });
  // 2) send_turn
  send('send_turn', { content, enable_thinking: enableThinking });
});

ws.on('message', (data) => {
  let msg;
  try { msg = JSON.parse(data.toString()); } catch (e) { console.log('[raw]', data.toString()); return; }
  if (msg.method) {
    // notification
    const p = msg.params || {};
    switch (msg.method) {
      case 'turn_started':
        console.log('--- turn_started ---');
        console.log('prompt_tokens:', p.prompt_tokens, 'n_ctx:', p.n_ctx);
        console.log('PROMPT (first 600 chars):\n' + String(p.prompt).slice(0, 600));
        console.log('PROMPT (last 200 chars):\n' + String(p.prompt).slice(-200));
        console.log('--- streaming ---');
        break;
      case 'turn_chunk':
        replyText += p.text || '';
        tokenCount += (p.tokens ? p.tokens.length : 0);
        process.stdout.write(p.text || '');
        break;
      case 'turn_done':
        console.log('\n--- turn_done --- totalReplyChars=' + replyText.length + ' tokenInfos=' + tokenCount);
        clearTimeout(timeout); ws.close(); process.exit(0);
        break;
      case 'turn_error':
        console.log('\n--- turn_error ---', JSON.stringify(p));
        clearTimeout(timeout); ws.close(); process.exit(1);
        break;
      case 'turn_rejected':
        console.log('\n--- turn_rejected ---', JSON.stringify(p));
        clearTimeout(timeout); ws.close(); process.exit(0);
        break;
      default:
        console.log('[notif]', msg.method, JSON.stringify(p).slice(0, 200));
    }
  } else if (msg.id !== undefined) {
    // response to a request
    if (msg.result && msg.result.tokens) {
      console.log('[tokenize result] count=' + msg.result.count + ' tokens=' + JSON.stringify(msg.result.tokens));
    } else if (msg.error) {
      console.log('[rpc error]', JSON.stringify(msg.error));
    } // send_turn's own response arrives last with result null — ignore
  }
});

ws.on('error', (e) => { console.error('[ws error]', e.message); clearTimeout(timeout); process.exit(3); });
ws.on('close', () => { /* no-op */ });
