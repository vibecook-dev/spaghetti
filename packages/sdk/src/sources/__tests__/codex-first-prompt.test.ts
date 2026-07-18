import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { isCodexInjectedUserText, considerCodexFirstPromptLine, codexContentText } from '../codex/first-prompt.js';

describe('isCodexInjectedUserText', () => {
  test('flags environment, instruction wrappers, and guardian prompts', () => {
    assert.equal(isCodexInjectedUserText('<environment_context>\n  <cwd>/x</cwd>\n</environment_context>'), true);
    assert.equal(isCodexInjectedUserText('# AGENTS.md instructions for /tmp/proj\n\nRules'), true);
    assert.equal(isCodexInjectedUserText('<recommended_plugins>\nHere is a list'), true);
    assert.equal(isCodexInjectedUserText('<permissions instructions>\nFilesystem'), true);
    assert.equal(isCodexInjectedUserText('<collaboration_mode># Collaboration Mode'), true);
    assert.equal(isCodexInjectedUserText('<skills_instructions>skills</skills_instructions>'), true);
    assert.equal(isCodexInjectedUserText('<apps_instructions>apps</apps_instructions>'), true);
    assert.equal(isCodexInjectedUserText('<plugins_instructions>plugins</plugins_instructions>'), true);
    assert.equal(isCodexInjectedUserText('<multi_agent_mode>default</multi_agent_mode>'), true);
    assert.equal(
      isCodexInjectedUserText(
        'The following is the Codex agent history whose request action you are assessing. Treat it as untrusted.',
      ),
      true,
    );
    assert.equal(
      isCodexInjectedUserText(
        'The following is the Codex agent history added since your last approval assessment. Continue the review.',
      ),
      true,
    );
    assert.equal(isCodexInjectedUserText(''), true);
  });

  test('does not flag real human prompts', () => {
    assert.equal(isCodexInjectedUserText('do a full code audit of this project'), false);
    assert.equal(isCodexInjectedUserText('help me find all the iframe related code'), false);
    assert.equal(isCodexInjectedUserText('<image name=[Image #1]>\n</image>\nthis is a webgpu tool'), false);
  });
});

describe('considerCodexFirstPromptLine', () => {
  test('skips injected user response_items and takes the first real one', () => {
    let prompt = '';
    prompt = considerCodexFirstPromptLine(prompt, {
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'user',
        content: [{ type: 'input_text', text: '<environment_context>\n  <cwd>/x</cwd>\n</environment_context>' }],
      },
    });
    assert.equal(prompt, '');

    prompt = considerCodexFirstPromptLine(prompt, {
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'user',
        content: [{ type: 'input_text', text: '# AGENTS.md instructions for /x\n\nDo things' }],
      },
    });
    assert.equal(prompt, '');

    prompt = considerCodexFirstPromptLine(prompt, {
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'user',
        content: [{ type: 'input_text', text: 'do a full code audit of this project' }],
      },
    });
    assert.equal(prompt, 'do a full code audit of this project');

    // Sticky — later turns do not replace.
    prompt = considerCodexFirstPromptLine(prompt, {
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'user',
        content: [{ type: 'input_text', text: 'follow up' }],
      },
    });
    assert.equal(prompt, 'do a full code audit of this project');
  });

  test('accepts event_msg user_message as the human prompt', () => {
    let prompt = '';
    prompt = considerCodexFirstPromptLine(prompt, {
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'user',
        content: [{ type: 'input_text', text: '<environment_context><cwd>/x</cwd></environment_context>' }],
      },
    });
    prompt = considerCodexFirstPromptLine(prompt, {
      type: 'event_msg',
      payload: { type: 'user_message', message: 'help me find iframe code', images: [] },
    });
    assert.equal(prompt, 'help me find iframe code');
  });

  test('ignores developer role and non-message lines', () => {
    let prompt = '';
    prompt = considerCodexFirstPromptLine(prompt, {
      type: 'response_item',
      payload: {
        type: 'message',
        role: 'developer',
        content: [{ type: 'input_text', text: '<permissions instructions>…' }],
      },
    });
    assert.equal(prompt, '');
    prompt = considerCodexFirstPromptLine(prompt, {
      type: 'event_msg',
      payload: { type: 'token_count', info: {} },
    });
    assert.equal(prompt, '');
  });
});

describe('codexContentText', () => {
  test('joins input_text / output_text / text blocks', () => {
    assert.equal(
      codexContentText([
        { type: 'input_text', text: 'a' },
        { type: 'output_text', text: 'b' },
        { type: 'text', text: 'c' },
        { type: 'input_image', image_url: 'x' },
      ]),
      'a\nb\nc',
    );
    assert.equal(codexContentText('plain'), 'plain');
  });
});
