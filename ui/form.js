// A server's question halfway through a command (MCP elicitation), as the hub describes it:
// the form (`request.form`) drawn as fields and read back as the answer, and the page
// (`request.page`) shown as an address with its site set apart. Used by the question window
// and by clak-daemon's page, each with its own `el`.
//
// The checks here only say at once what is wrong: the hub checks the answer again, and its
// check is the one that counts. Nothing here is a link, and nothing answers on Enter.
"use strict";

const ClakForm = (() => {
  let count = 0;

  /// A local date and time ("2026-11-16T10:30") as RFC 3339, with this computer's offset.
  const withOffset = (local) => {
    const date = new Date(local);
    if (Number.isNaN(date.getTime())) return local;
    const pad = (n) => String(Math.floor(Math.abs(n))).padStart(2, "0");
    const minutes = -date.getTimezoneOffset();
    const seconds = local.length === 16 ? ":00" : "";
    return `${local}${seconds}${minutes >= 0 ? "+" : "-"}${pad(minutes / 60)}:${pad(minutes % 60)}`;
  };

  /// RFC 3339 as the field shows it, in this computer's time.
  const toLocal = (text) => {
    const date = new Date(text);
    if (Number.isNaN(date.getTime())) return "";
    const pad = (n) => String(n).padStart(2, "0");
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
  };

  const looksLikeEmail = (text) => {
    const parts = text.split("@");
    if (parts.length !== 2 || !parts[0] || /\s/.test(text)) return false;
    const domain = parts[1];
    return domain.includes(".") && !domain.startsWith(".") && !domain.endsWith(".");
  };

  const looksLikeUri = (text) => {
    try {
      new URL(text);
      return true;
    } catch {
      return false;
    }
  };

  const shown = (n) => String(n);

  /// One field: its label, its control, what it's for, and its error.
  const field = (f, el) => {
    const id = `field-${++count}`;
    const problem = el("div", { class: "field-error", role: "alert" });
    const describe = f.description ? el("div", { class: "field-hint quiet" }, f.description) : null;
    const optional = f.required ? null : el("span", { class: "field-optional" }, " (optional)");
    const label = () => el("label", { class: "field-label", for: id }, f.title, optional);
    let control;
    let read;
    let inputs = [];

    if (f.kind === "boolean") {
      const box = el("input", { type: "checkbox", id });
      box.checked = f.default === true;
      inputs = [box];
      control = el("label", { class: "field-check" }, box, el("span", null, f.title));
      read = () => ({ value: box.checked });
      return finish(el("div", { class: "field" }, control, describe, problem));
    }

    if (f.kind === "choice" && f.required && f.options.length <= 5) {
      const name = `${id}-choice`;
      const radios = f.options.map((o) => {
        const radio = el("input", { type: "radio", name, value: o.value });
        radio.checked = f.default === o.value;
        inputs.push(radio);
        return el("label", { class: "field-check" }, radio, el("span", null, o.label));
      });
      control = el("div", { class: "field-options", role: "radiogroup" }, radios);
      read = () => {
        const chosen = inputs.find((r) => r.checked);
        return chosen ? { value: chosen.value } : { error: `“${f.title}” is required.` };
      };
      return finish(el("div", { class: "field" }, el("div", { class: "field-label" }, f.title), control, describe, problem));
    }

    if (f.kind === "choice") {
      const select = el("select", { id });
      select.append(el("option", { value: "" }, f.required ? "Choose…" : "—"));
      for (const o of f.options) {
        const option = el("option", { value: o.value }, o.label);
        option.selected = f.default === o.value;
        select.append(option);
      }
      inputs = [select];
      control = select;
      read = () => {
        if (!select.value) return f.required ? { error: `“${f.title}” is required.` } : {};
        return { value: select.value };
      };
      return finish(el("div", { class: "field" }, label(), control, describe, problem));
    }

    if (f.kind === "choices") {
      const chosen = Array.isArray(f.default) ? f.default : [];
      const boxes = f.options.map((o) => {
        const box = el("input", { type: "checkbox", value: o.value });
        box.checked = chosen.includes(o.value);
        inputs.push(box);
        return el("label", { class: "field-check" }, box, el("span", null, o.label));
      });
      control = el("div", { class: "field-options many" }, boxes);
      read = () => {
        const values = inputs.filter((b) => b.checked).map((b) => b.value);
        if (!values.length) return f.required ? { error: `“${f.title}” is required.` } : {};
        if (f.min_items != null && values.length < f.min_items) return { error: `“${f.title}”: choose at least ${f.min_items}.` };
        if (f.max_items != null && values.length > f.max_items) return { error: `“${f.title}”: choose at most ${f.max_items}.` };
        return { value: values };
      };
      return finish(el("div", { class: "field" }, el("div", { class: "field-label" }, f.title, optional), control, describe, problem));
    }

    if (f.kind === "number") {
      const input = el("input", { type: "number", id, step: f.integer ? "1" : "any", inputmode: f.integer ? "numeric" : "decimal" });
      if (f.minimum != null) input.min = f.minimum;
      if (f.maximum != null) input.max = f.maximum;
      if (typeof f.default === "number") input.value = f.default;
      inputs = [input];
      control = input;
      read = () => {
        const text = input.value.trim();
        if (!text) return f.required ? { error: `“${f.title}” is required.` } : {};
        const n = Number(text);
        if (!Number.isFinite(n)) return { error: `“${f.title}” must be a number.` };
        if (f.integer && !Number.isInteger(n)) return { error: `“${f.title}” must be a whole number.` };
        if (f.minimum != null && n < f.minimum) return { error: `“${f.title}” must be at least ${shown(f.minimum)}.` };
        if (f.maximum != null && n > f.maximum) return { error: `“${f.title}” must be at most ${shown(f.maximum)}.` };
        return { value: n };
      };
      return finish(el("div", { class: "field" }, label(), control, describe, problem));
    }

    // Text, with its format.
    const type = { email: "email", uri: "url", date: "date", "date-time": "datetime-local" }[f.format] || "text";
    const long = type === "text" && f.max_length > 200;
    const input = long ? el("textarea", { id, rows: "3" }) : el("input", { type, id, spellcheck: "false" });
    if (f.max_length) input.maxLength = f.max_length;
    if (typeof f.default === "string") input.value = f.format === "date-time" ? toLocal(f.default) : f.default;
    inputs = [input];
    control = input;
    read = () => {
      const text = input.value;
      if (!text) return f.required ? { error: `“${f.title}” is required.` } : {};
      const length = [...text].length;
      if (f.min_length != null && length < f.min_length) return { error: `“${f.title}” needs at least ${f.min_length} characters.` };
      if (f.max_length != null && length > f.max_length) return { error: `“${f.title}” takes at most ${f.max_length} characters.` };
      if (f.pattern) {
        let pattern = null;
        try {
          pattern = new RegExp(f.pattern, "u");
        } catch {
          // Written for another engine: the hub checks it.
        }
        if (pattern && !pattern.test(text)) return { error: `“${f.title}” isn't in the form the server expects.` };
      }
      if (f.format === "email" && !looksLikeEmail(text)) return { error: `“${f.title}” must be an email address.` };
      if (f.format === "uri" && !looksLikeUri(text)) return { error: `“${f.title}” must be an address (URI).` };
      if (f.format === "date" && !/^\d{4}-\d{2}-\d{2}$/.test(text)) return { error: `“${f.title}” must be a date (YYYY-MM-DD).` };
      if (f.format === "date-time") return { value: withOffset(text) };
      return { value: text };
    };
    return finish(el("div", { class: "field" }, label(), control, describe, problem));

    function finish(node) {
      return {
        field: f,
        node,
        read,
        error: (message) => {
          problem.textContent = message || "";
          node.classList.toggle("wrong", !!message);
        },
        disable: (off) => {
          for (const input of inputs) input.disabled = off;
        },
      };
    }
  };

  /// The form: `node` to show, `read()` for the answer and its first error, `disable(off)`.
  const render = (form, el) => {
    const rows = (form?.fields || []).map((f) => field(f, el));
    return {
      node: el("div", { class: "form" }, rows.map((r) => r.node)),
      empty: rows.length === 0,
      read: () => {
        const content = {};
        let error = null;
        for (const row of rows) {
          const result = row.read();
          row.error(result.error);
          if (result.error && !error) error = result.error;
          if (result.value !== undefined) content[row.field.name] = result.value;
        }
        return { content, error };
      },
      disable: (off) => rows.forEach((r) => r.disable(off)),
    };
  };

  /// The page's address, whole, with the site set apart. Not a link: the window opens it.
  const address = (page, el) =>
    el("div", { class: "address selectable" },
      el("span", { class: "address-part" }, page.before),
      el("strong", { class: "address-host" }, page.host),
      el("span", { class: "address-part" }, page.after));

  return { render, address };
})();
