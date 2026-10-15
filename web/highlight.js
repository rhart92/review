'use strict';
(() => {
  const languages = new Map([
    ['ts', 'typescript'], ['tsx', 'tsx'], ['mts', 'typescript'], ['cts', 'typescript'],
    ['js', 'javascript'], ['jsx', 'jsx'], ['mjs', 'javascript'], ['cjs', 'javascript'],
    ['json', 'json'], ['jsonc', 'json'], ['jsonl', 'json'],
    ['rs', 'rust'], ['py', 'python'], ['pyw', 'python'], ['pyi', 'python'],
    ['go', 'go'], ['java', 'java'], ['c', 'c'], ['h', 'cpp'],
    ['cc', 'cpp'], ['cpp', 'cpp'], ['cxx', 'cpp'], ['hpp', 'cpp'], ['hh', 'cpp'], ['hxx', 'cpp'],
    ['rb', 'ruby'], ['rake', 'ruby'], ['gemspec', 'ruby'],
    ['css', 'css'], ['scss', 'scss'], ['html', 'markup'], ['htm', 'markup'], ['xml', 'markup'], ['svg', 'markup'],
    ['xhtml', 'markup'], ['vue', 'markup'], ['svelte', 'markup'],
    ['yaml', 'yaml'], ['yml', 'yaml'], ['toml', 'toml'], ['ini', 'ini'], ['cfg', 'ini'],
    ['sh', 'bash'], ['bash', 'bash'], ['zsh', 'bash'],
    ['md', 'markdown'], ['markdown', 'markdown'], ['mdown', 'markdown'],
    ['sql', 'sql'], ['dockerfile', 'docker'], ['graphql', 'graphql'], ['gql', 'graphql'],
  ]);
  const filenames = new Map([
    ['dockerfile', 'docker'], ['containerfile', 'docker'],
    ['gemfile', 'ruby'], ['rakefile', 'ruby'], ['.bashrc', 'bash'], ['.bash_profile', 'bash'],
    ['.zshrc', 'bash'], ['.profile', 'bash'], ['.gitconfig', 'ini'],
  ]);

  function languageFor(path) {
    const name = path.split('/').at(-1).toLowerCase();
    if (filenames.has(name)) return filenames.get(name);
    if (/^(dockerfile|containerfile)\./.test(name)) return 'docker';
    return languages.get(name.slice(name.lastIndexOf('.') + 1));
  }

  // Keep the innermost semantic scope: combining parent/child CSS classes
  // makes a JSX tag's color override its attribute values and expressions.
  function splitTokens(stream) {
    const lines = [[]];
    function walk(value, className = '') {
      if (typeof value === 'string') {
        const pieces = value.split('\n');
        for (let index = 0; index < pieces.length; index++) {
          if (index) lines.push([]);
          const text = pieces[index];
          if (!text) continue;
          const tokens = lines[lines.length - 1], last = tokens[tokens.length - 1];
          if (last && last.className === className) last.text += text;
          else tokens.push({ text, className });
        }
      } else if (Array.isArray(value)) {
        for (const child of value) walk(child, className);
      } else {
        const aliases = Array.isArray(value.alias) ? value.alias : value.alias ? [value.alias] : [];
        walk(value.content, ['token', value.type, ...aliases].join(' '));
      }
    }
    walk(stream);
    return lines;
  }

  function highlightRun(records, language, side, result) {
    if (!records.length || (side === 'old' && !records.some(record => record.kind === 'del'))) return;
    const source = records.map(record => record.text).join('\n');
    const env = { code: source, grammar: window.Prism.languages[language], language };
    window.Prism.hooks.run('before-tokenize', env);
    env.tokens = window.Prism.tokenize(env.code, env.grammar);
    window.Prism.hooks.run('after-tokenize', env);
    const lines = splitTokens(env.tokens);
    for (let index = 0; index < records.length; index++) {
      const record = records[index];
      if (side === 'old' && record.kind !== 'del') continue;
      result.set(record, lines[index] || []);
    }
  }

  function tokens(path, groups) {
    const result = new Map();
    const language = languageFor(path);
    if (!language) return result;
    for (const group of groups) {
      for (const side of ['old', 'new']) {
        let run = [];
        let previous = null;
        for (const record of group) {
          if (record.kind !== 'context' && record.kind !== (side === 'old' ? 'del' : 'add')) continue;
          const number = record[side];
          if (previous !== null && number !== null && number !== previous + 1) {
            highlightRun(run, language, side, result);
            run = [];
          }
          run.push(record);
          previous = number;
        }
        highlightRun(run, language, side, result);
      }
    }
    return result;
  }

  window.ReviewHighlight = Object.freeze({ tokens });
})();
