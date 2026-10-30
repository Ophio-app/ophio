// SPDX-License-Identifier: Apache-2.0
(() => {
  const memory = globalThis.__voiceControls ??= { next: 1, ids: new WeakMap(), nodes: new Map() };
  const result = [];
  memory.nodes.clear();
  const ax = [];
  const visit = (root, ox, oy) => {
    for (const e of root.querySelectorAll('*')) {
      if (e.shadowRoot) visit(e.shadowRoot, ox, oy);
      if (e.tagName === 'IFRAME') {
        try { const box = e.getBoundingClientRect(); if (e.contentDocument) visit(e.contentDocument, ox + box.x + e.clientLeft, oy + box.y + e.clientTop); } catch { /* Cross-origin frames require their own execution context. */ }
      }
      const tag = e.tagName.toLowerCase();
      if (!['button', 'a', 'input', 'textarea', 'select', 'summary'].includes(tag) && !e.hasAttribute('role') && !e.isContentEditable && e.tabIndex < 0) continue;
      if (tag === 'input' && e.type === 'hidden') continue;
      const rect = e.getBoundingClientRect();
      const style = e.ownerDocument.defaultView.getComputedStyle(e);
      if (rect.width <= 0 || rect.height <= 0 || style.visibility === 'hidden' || style.display === 'none' || e.getAttribute('aria-hidden') === 'true') continue;
      let id = memory.ids.get(e);
      if (!id) { id = `e${memory.next++}`; memory.ids.set(e, id); }
      memory.nodes.set(id, e);
      const field = ['input', 'textarea', 'select'].includes(tag) || e.isContentEditable;
      const referenced = (e.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean).map(id => e.ownerDocument.getElementById(id)).filter(Boolean);
      // Never inspect input values or editable text, even when building labels.
      const labelText = node => { const walker = node.ownerDocument.createTreeWalker(node, NodeFilter.SHOW_TEXT); let text = ''; while(walker.nextNode()) { const n=walker.currentNode; if (!n.parentElement?.closest('input,textarea,select,[contenteditable]')) text += n.textContent || ''; if(text.length>512) break; } return text.slice(0,256); };
      const name = e.getAttribute('aria-label') || referenced.map(labelText).join(' ') || (e.labels ? [...e.labels].map(labelText).join(' ') : '') || (!field ? labelText(e) : '') || e.getAttribute('placeholder') || e.getAttribute('title') || '';
      const role = e.getAttribute('role') || ({button:'button', a:'link', input: ['checkbox','radio'].includes(e.type) ? e.type : ['submit','reset','button'].includes(e.type) ? 'button' : 'textbox', textarea:'textbox', select:'combobox', summary:'button'}[tag]) || (e.isContentEditable ? 'textbox' : 'control');
      const metadata = [e.type, e.autocomplete, e.name, e.id, name].join(' ').toLowerCase();
      const sensitive = e.type === 'password' || /password|passcode|secret|token|credit|card|cc-|cvc|cvv|ssn|one.time.code|api[ _-]?key|credential|authorization|private[ _-]?key|recovery|seed|otp/.test(metadata);
      if (!field && !sensitive && ['button','a'].includes(tag)) ax.push(id);
      const destructive = ['submit','reset'].includes(e.type) || /\b(submit|send|buy|pay|delete|purchase|checkout|close|remove)\b/i.test(name);
      result.push({id,role,name:name.slice(0,128),bounds:{x:rect.x+ox,y:rect.y+oy,width:rect.width,height:rect.height},visible:rect.bottom+oy>0&&rect.right+ox>0&&rect.top+oy<innerHeight&&rect.left+ox<innerWidth,enabled:!e.disabled&&e.getAttribute('aria-disabled')!=='true',focused:e.ownerDocument.activeElement===e,sensitive,destructive});
      if (result.length >= 1000) break;
    }
  };
  visit(document, 0, 0);
  result.sort((a,b) => Number(b.visible)-Number(a.visible) || Number(b.focused)-Number(a.focused));
  return { elements: result.slice(0,100), ax, context: `${location.href}|${performance.timeOrigin}` };
})()
