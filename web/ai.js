import { $t, $tn, serverMessage } from "./i18n.js";
import { splitMarkdown } from "./markdown.js";
const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
let controller = null;
let generation = 0;
export function stopAI() {
  generation++;
  controller?.abort();
  controller = null;
}
const presets = {
  chatgpt: { label: "ChatGPT", endpoint: "https://api.openai.com/v1" },
  openai: { label: "OpenAI", endpoint: "https://api.openai.com/v1" },
  openrouter: { label: "OpenRouter", endpoint: "https://openrouter.ai/api/v1" },
  deepseek: { label: "DeepSeek", endpoint: "https://api.deepseek.com" },
  anthropic: {
    label: "Anthropic Claude",
    endpoint: "https://api.anthropic.com/v1",
  },
  compatible: { label: "OpenAI-compatible API", endpoint: "" },
};

const serverTools = [
  ["openrouter:web_search", () => $t("Web Search")],
  ["openrouter:web_fetch", () => $t("Web Fetch")],
  ["openrouter:image_generation", () => $t("Image Generation")],
  ["openrouter:datetime", () => $t("Datetime")],
  ["openrouter:fusion", () => $t("Fusion")],
  ["openrouter:advisor", () => $t("Advisor")],
  ["openrouter:subagent", () => $t("Subagent")],
  ["openrouter:shell", () => $t("Hosted Shell")],
  ["minerva:shell", () => $t("Desktop Shell")],
];

export async function renderAISettings({
  api,
  esc,
  data,
  error,
  localMode = false,
}) {
  const epoch = generation;
  const providers = await api("ai/providers");
  if (epoch !== generation) return;
  $("#breadcrumb").textContent = $t("AI providers");
  $("#content").innerHTML =
    `<section class="page"><div class="eyebrow">${$t("IN-APP INTELLIGENCE")}</div><div class="page-heading"><div><h1>${$t("AI providers")}</h1><p class="subtitle">${localMode ? $t("Choose private models for this computer.") : $t("Choose the models your server can use for contextual chat.")}</p></div>${data.user.admin ? `<button class="btn primary" id="add-provider">${$t("+ Add provider")}</button>` : ""}</div>
    <div class="panel ai-notice"><h2>${$t("Your context, your choice")}</h2><p>${$t("Chat sends the question, recent conversation, and permitted notes and records from the selected scope to your chosen provider. Files attached to a question are sent with that question only, and are not stored. With “No context”, no notes are sent. Provider usage may incur API charges.")}</p><p>${localMode ? $t("Private credentials are encrypted on this computer and are never synced to a Minerva server. Requests go directly to your chosen provider. ChatGPT sign-in uses eligible subscription allowances, separately from API billing.") : $t("API keys are encrypted on the server and never returned to this page. ChatGPT subscriptions do not supply OpenAI API credits. These settings are separate from external MCP connections.")}</p></div>
    <div class="ai-provider-grid">${providers.map((p) => `<section class="panel ai-provider"><span class="pill">${esc(presets[p.kind]?.label || p.kind)}</span><h2>${esc(p.name)}</h2><p>${esc(p.model)}</p><small>${esc(p.endpoint)}</small><p>${localMode ? $t("Private · this computer only") : $t("Available to server members · key stored{value} · answers up to {date} tokens", { value: p.web_search ? $t(" · web search on") : "", date: Number(p.max_tokens || 4096).toLocaleString() })}</p>${data.user.admin ? `<div class="ai-provider-actions"><button class="btn" data-edit-provider="${p.id}">${$t("Edit")}</button>${p.kind === "chatgpt" ? `<button class="btn" data-signout-provider="${p.id}">${$t("Sign out of ChatGPT")}</button>` : ""}<button class="btn danger" data-remove-provider="${p.id}">${$t("Remove provider")}</button></div>` : ""}</section>`).join("") || `<div class="empty"><h3>${$t("No providers configured")}</h3><p>${localMode ? $t("Add your own model or continue with ChatGPT to enable private AI.") : $t("A server administrator can add a model and API key to enable chat.")}</p></div>`}</div></section>`;
  // Adds a provider, or edits `existing`. The stored key is never shown: an
  // empty key field keeps it, and pointing it at another server needs it again.
  const providerForm = (existing = null) => {
    const modal = $("#modal");
    const p = existing || { kind: "openai", endpoint: presets.openai.endpoint };
    modal.innerHTML = `<form id="provider-form"><h2>${existing ? $t("Edit AI provider") : $t("Add an AI provider")}</h2><p>${localMode ? $t("This private profile is stored on this computer. Use the personal workspace or an offline copy to send requests directly to your own model.") : $t("This profile is available to server members. Requests use this API key and its billing account.")}</p><label>${$t("Provider")}<select name="kind">${Object.entries(
      presets,
    )
      .filter(([k]) => k !== "chatgpt" || localMode)
      .map(
        ([k, preset]) =>
          `<option value="${k}" ${k === p.kind ? "selected" : ""}>${esc(preset.label)}</option>`,
      )
      .join(
        "",
      )}</select></label><label>${$t("Profile name")}<input name="name" maxlength="200" placeholder="${$t("e.g. Team assistant")}" value="${esc(p.name || "")}" required></label><label>${$t("API base URL")}<input name="endpoint" type="url" value="${esc(p.endpoint || "")}" required></label><label>${$t("Model ID")}<input name="model" list="provider-models" placeholder="${$t("Exact model ID from your provider")}" maxlength="200" value="${esc(p.model || "")}" required></label><datalist id="provider-models"></datalist><div id="openrouter-setup" hidden><button type="button" class="btn" id="discover-models">${$t("Load available models and tools")}</button><p id="discovery-status" role="status"></p><p id="model-capabilities" class="hint"></p><fieldset><legend>${$t("Allowed server tools")}</legend>${serverTools
      .filter(([id]) => id !== "minerva:shell" || localMode)
      .map(
        ([id, label]) =>
          `<label class="checkbox-label"><input type="checkbox" data-server-tool="${id}" ${(p.server_tools || []).includes(id) ? "checked" : ""}>${label()}</label>`,
      )
      .join(
        "",
      )}</fieldset><p class="hint">${$t("OpenRouter tools run remotely and incur additional charges. Availability depends on the model, route and region. Fusion requires Web Search and Web Fetch. Hosted Shell runs remotely, not on this computer.")}</p></div><label class="checkbox-label"><input type="checkbox" name="web_search" ${p.web_search ? "checked" : ""}>${$t("Allow web search")}</label><p class="hint web-search-hint"></p><label>${$t("Max answer length (tokens)")}<input name="max_tokens" type="number" min="256" max="131072" step="256" value="${esc(String(p.max_tokens || 4096))}" required></label><p class="hint">${$t("Thinking models (Qwen, DeepSeek R1…) also spend these tokens reasoning: give them 16384 or more, within the model's context window. 4096 suits most cloud models and keeps costs predictable.")}</p><label>${$t("API key")}<input name="api_key" type="password" autocomplete="new-password" maxlength="4096" ${existing ? 'placeholder="Leave empty to keep the stored key"' : "required"}></label>${existing ? `<p class="stored-key-hint">${$t("The stored key is kept unless you enter a new one. Changing the API base URL to another server requires entering the key again.")}</p>` : ""}<p class="endpoint-hint">${$t("Use the provider’s API base URL, or the address of a self-hosted or compatible server. Questions and permitted note content are sent there, so use HTTPS unless the server is on a trusted network. Keep a backup of the server’s .ai-key file alongside the database.")}</p><div class="form-error" role="alert"></div><footer><button type="button" class="btn" id="cancel-provider">${$t("Cancel")}</button><button class="btn primary" type="submit">${$t("Save provider")}</button></footer></form>`;
    let oauthFlow = null;
    let oauthTimer = null;
    if (localMode) {
      const keyLabel = $("input[name=api_key]", modal).closest("label");
      keyLabel.insertAdjacentHTML(
        "beforebegin",
        `<label id="provider-auth">${$t("Authentication")}<select name="auth_mode"><option value="api_key">${$t("API key")}</option><option value="none" ${p.auth_mode === "none" ? "selected" : ""}>${$t("No authentication (self-hosted)")}</option></select></label>`,
      );
      keyLabel.insertAdjacentHTML(
        "afterend",
        `<div id="chatgpt-setup" hidden><p>${$t("Continue with ChatGPT in your browser. Eligible plans can share subscription usage with Minerva; availability and limits are controlled by OpenAI. No API-key fallback is used.")}</p><button type="button" class="btn" id="chatgpt-connect" data-oai-siwc-button>${$t("Continue with ChatGPT")}</button><button type="button" class="btn" id="chatgpt-models" ${existing?.kind === "chatgpt" ? "" : "hidden"}>${$t("Load available models")}</button><p id="chatgpt-status" role="status"></p></div>`,
      );
      $("#chatgpt-models", modal).onclick = async () => {
        const form = $("#provider-form", modal);
        try {
          const result = await api(`ai/chatgpt/${existing.id}/models`, "POST");
          if (!form.isConnected || !modal.open) return;
          $("#provider-models", modal).innerHTML = result.models
            .map((m) => `<option value="${esc(m.id)}">${esc(m.name)}</option>`)
            .join("");
          $("#chatgpt-status", modal).textContent = $t(
            "Choose an available model ID above.",
          );
        } catch (err) {
          if (form.isConnected && modal.open)
            $("#chatgpt-status", modal).textContent = err.message;
        }
      };
      $("#chatgpt-connect", modal).onclick = async () => {
        const name = $("input[name=name]", modal).value.trim();
        if (!name) {
          $("input[name=name]", modal).reportValidity();
          return;
        }
        const form = $("#provider-form", modal);
        const button = $("#chatgpt-connect", modal);
        button.disabled = true;
        try {
          const result = await api("ai/chatgpt/start", "POST", {
            name,
            ...(existing?.kind === "chatgpt"
              ? { provider_id: existing.id }
              : {}),
          });
          if (
            !form.isConnected ||
            !modal.open ||
            $("select[name=kind]", modal).value !== "chatgpt"
          ) {
            await api(`ai/chatgpt/flows/${result.flow_id}`, "DELETE");
            return;
          }
          oauthFlow = result.flow_id;
          const expectedFlow = oauthFlow;
          const status = $("#chatgpt-status", modal);
          status.innerHTML = `<a class="btn primary" href="${esc(result.url)}">${$t("Open ChatGPT sign-in in your browser")}</a><p>${$t("Waiting for sign-in. Return here after approving access.")}</p>`;
          const poll = async () => {
            if (oauthFlow !== expectedFlow || !modal.open) return;
            try {
              const state = await api(`ai/chatgpt/flows/${oauthFlow}`);
              if (oauthFlow !== expectedFlow || !modal.open) return;
              if (state.status === "connected") {
                clear();
                await renderAISettings({ api, esc, data, error, localMode });
                return;
              }
              if (state.status === "failed")
                throw new Error(
                  $t("ChatGPT sign-in failed. Please reconnect your account."),
                );
              oauthTimer = setTimeout(poll, 1500);
            } catch (err) {
              if (oauthFlow !== expectedFlow || !form.isConnected) return;
              status.textContent = err.message;
              if (oauthFlow)
                api(`ai/chatgpt/flows/${oauthFlow}`, "DELETE").catch(() => {});
              oauthFlow = null;
              button.disabled = false;
            }
          };
          oauthTimer = setTimeout(poll, 1500);
        } catch (err) {
          if (!form.isConnected || !modal.open) return;
          $("#chatgpt-status", modal).textContent = err.message;
          button.disabled = false;
        }
      };
    }
    if (localMode) {
      const settings = document.createElement("button");
      settings.type = "button";
      settings.className = "btn";
      settings.textContent = $t("Desktop Shell settings");
      $("#openrouter-setup", modal).append(settings);
      settings.onclick = () => configureShell(api, esc, error);
    }
    modal.showModal();
    const clear = () => {
      discoveryGeneration++;
      modal.close();
      modal.innerHTML = "";
    };
    $("#cancel-provider").onclick = clear;
    modal.addEventListener(
      "close",
      () => {
        clearTimeout(oauthTimer);
        if (oauthFlow)
          api(`ai/chatgpt/flows/${oauthFlow}`, "DELETE").catch(() => {});
        oauthFlow = null;
        const key = $("input[name=api_key]", modal);
        if (key) key.value = "";
      },
      { once: true },
    );
    // Only OpenAI and Anthropic have a web search tool of their own.
    const searchable = () => {
      const kind = $("select[name=kind]", modal).value;
      const oauth = kind === "chatgpt";
      const auth = $("select[name=auth_mode]", modal);
      if (auth) {
        $("#provider-auth", modal).hidden = kind !== "compatible";
        if (kind !== "compatible") auth.value = "api_key";
        $("#chatgpt-setup", modal).hidden = !oauth;
      }
      const key = $("input[name=api_key]", modal);
      key.required = !existing && !oauth && auth?.value !== "none";
      key.closest("label").hidden = oauth || auth?.value === "none";
      if (oauth || auth?.value === "none") key.value = "";
      $("input[name=endpoint]", modal).readOnly = oauth;
      $("input[name=endpoint]", modal).closest("label").hidden = oauth;
      $("input[name=model]", modal).closest("label").hidden =
        oauth && existing?.kind !== "chatgpt";
      $(".endpoint-hint", modal).hidden = oauth;
      const storedHint = $(".stored-key-hint", modal);
      if (storedHint) storedHint.hidden = oauth || auth?.value === "none";
      $("input[name=max_tokens]", modal).closest("label").hidden = oauth;
      $("input[name=max_tokens]", modal).closest(
        "label",
      ).nextElementSibling.hidden = oauth;
      $("input[name=model]", modal).required =
        !oauth || existing?.kind === "chatgpt";
      $("button[type=submit]", modal).hidden =
        oauth && existing?.kind !== "chatgpt";
      if (existing?.kind === "chatgpt")
        $("select[name=kind]", modal).disabled = true;
      const box = $("input[name=web_search]", modal);
      const supported = kind === "openai" || kind === "anthropic";
      $("#openrouter-setup", modal).hidden = kind !== "openrouter";
      box.closest("label").hidden = kind === "openrouter" || oauth;
      $(".web-search-hint", modal).hidden = kind === "openrouter" || oauth;
      box.disabled = !supported;
      if (!supported) box.checked = false;
      $(".web-search-hint", modal).textContent = supported
        ? $t(
            "The model may search the web while answering. Searches run at the provider, are billed per search, and receive search terms based on the question and notes.",
          )
        : $t("Web search is available for OpenAI and Anthropic providers.");
    };
    let models = [];
    let discoveryGeneration = 0;
    const describeModel = () => {
      const model = models.find(
        (m) => m.id === $("input[name=model]", modal).value,
      );
      $("#model-capabilities", modal).textContent = model
        ? $t("Context: {context} tokens · Input: {input} · Output: {output}", {
            context: model.context_length ?? "?",
            input: (model.input_modalities || []).join(", "),
            output: (model.output_modalities || []).join(", "),
          })
        : "";
    };
    $("input[name=model]", modal).oninput = describeModel;
    $("#discover-models", modal).onclick = async () => {
      const run = ++discoveryGeneration;
      const form = $("#provider-form", modal);
      const button = $("#discover-models", modal);
      const values = Object.fromEntries(
        new FormData($("#provider-form", modal)),
      );
      if (existing) values.provider_id = existing.id;
      button.disabled = true;
      $("#discovery-status", modal).textContent = $t(
        "Loading models and tools…",
      );
      try {
        const result = await api("ai/providers/discover", "POST", values);
        if (
          !form.isConnected ||
          $("#provider-form", modal) !== form ||
          run !== discoveryGeneration ||
          !modal.open ||
          $("input[name=endpoint]", modal).value !== values.endpoint ||
          $("select[name=kind]", modal).value !== values.kind
        )
          return;
        models = result.models;
        $("#provider-models", modal).innerHTML = models
          .map((m) => `<option value="${esc(m.id)}">${esc(m.name)}</option>`)
          .join("");
        $("#discovery-status", modal).textContent = result.tools_available
          ? $t(
              "Loaded {count} models. Choose a model ID below or enter one manually.",
              { count: models.length },
            )
          : $t(
              "Models loaded. The tool catalog is unavailable; tool choices remain unchanged.",
            );
        if (result.tools_available) {
          $$("[data-server-tool]", modal).forEach((box) => {
            const found = result.tools.find(
              (t) => t.id === box.dataset.serverTool,
            );
            box.closest("label").title = found
              ? found.summary || ""
              : $t("Not listed in the current provider catalog");
          });
        }
        describeModel();
      } catch (err) {
        if (
          run === discoveryGeneration &&
          modal.open &&
          form.isConnected &&
          $("#provider-form", modal) === form
        )
          $("#discovery-status", modal).textContent =
            `${err.message} ${$t("You can still enter a model ID manually.")}`;
      } finally {
        if (run === discoveryGeneration) button.disabled = false;
      }
    };
    searchable();
    $("select[name=auth_mode]", modal)?.addEventListener("change", searchable);
    $("select[name=kind]", modal).onchange = (e) => {
      if (oauthFlow)
        api(`ai/chatgpt/flows/${oauthFlow}`, "DELETE").catch(() => {});
      oauthFlow = null;
      clearTimeout(oauthTimer);
      if (localMode) $("#chatgpt-connect", modal).disabled = false;
      discoveryGeneration++;
      models = [];
      $("#provider-models", modal).innerHTML = "";
      $("#discover-models", modal).disabled = false;
      $("#discovery-status", modal).textContent = "";
      $("#model-capabilities", modal).textContent = "";
      $("input[name=endpoint]", modal).value = presets[e.target.value].endpoint;
      searchable();
    };
    $("#provider-form").onsubmit = async (e) => {
      e.preventDefault();
      const button = $("button[type=submit]", modal);
      button.disabled = true;
      try {
        const values = Object.fromEntries(new FormData(e.target));
        if (existing?.kind === "chatgpt") values.kind = "chatgpt";
        values.web_search = $("input[name=web_search]", modal).checked;
        values.server_tools =
          values.kind === "openrouter"
            ? $$("[data-server-tool]:checked", modal).map(
                (box) => box.dataset.serverTool,
              )
            : [];
        values.web_search =
          values.kind === "openrouter"
            ? values.server_tools.includes("openrouter:web_search")
            : values.web_search;
        values.max_tokens = Number(values.max_tokens);
        if (existing) await api(`ai/providers/${existing.id}`, "PATCH", values);
        else await api("ai/providers", "POST", values);
        clear();
        await renderAISettings({ api, esc, data, error, localMode });
      } catch (err) {
        $(".form-error", modal).textContent = err.message;
        button.disabled = false;
      }
    };
  };
  $("#add-provider")?.addEventListener("click", () => providerForm());
  $$("[data-edit-provider]").forEach(
    (button) =>
      (button.onclick = () =>
        providerForm(
          providers.find((p) => p.id === button.dataset.editProvider),
        )),
  );
  $$("[data-signout-provider]").forEach((button) => {
    button.onclick = async () => {
      try {
        const result = await api(
          `ai/chatgpt/${button.dataset.signoutProvider}/signout`,
          "POST",
        );
        button.textContent = result.revoked
          ? $t("Signed out")
          : $t("Signed out locally. Revoke access in ChatGPT settings.");
        button.disabled = true;
      } catch (err) {
        error(err);
      }
    };
  });
  $$("[data-remove-provider]").forEach(
    (button) =>
      (button.onclick = async () => {
        const modal = $("#modal");
        modal.innerHTML = `<h2>${$t("Remove this provider?")}</h2><p>${$t("New requests will stop using this profile. Existing conversation history stays available.")}</p><footer><button class="btn" id="cancel-remove">${$t("Cancel")}</button><button class="btn danger" id="confirm-remove">${$t("Remove")}</button></footer>`;
        modal.showModal();
        $("#cancel-remove").onclick = () => modal.close();
        $("#confirm-remove").onclick = async () => {
          try {
            if (
              providers.find((p) => p.id === button.dataset.removeProvider)
                ?.kind === "chatgpt"
            ) {
              const result = await api(
                `ai/chatgpt/${button.dataset.removeProvider}/signout`,
                "POST",
              );
              if (!result.revoked)
                error(
                  new Error(
                    $t(
                      "Signed out locally. Revoke access in ChatGPT settings.",
                    ),
                  ),
                );
            }
            await api(
              `ai/providers/${button.dataset.removeProvider}`,
              "DELETE",
            );
            modal.close();
            await renderAISettings({ api, esc, data, error, localMode });
          } catch (err) {
            error(err);
          }
        };
      }),
  );
}

export async function renderAIChat({
  localMode = false,
  api,
  esc,
  data,
  workspaceId,
  initialScope,
  docked = false,
  dockedTitle = "",
  openNote,
  openDatabase,
  sendToNote,
  noteFromReply,
  canEditNote,
  configure,
  error,
}) {
  const epoch = generation;
  const [providers, history] = await Promise.all([
    api("ai/providers"),
    api("ai/chats"),
  ]);
  if (epoch !== generation) return;
  let chat = null,
    running = false;
  // A docked chat (a tab beside a note) stays on its one context.
  const scopeOptions = docked
    ? [
        {
          kind: initialScope.kind,
          id: initialScope.id,
          label: `${initialScope.kind === "note" ? "Note" : "Folder"} · ${dockedTitle}`,
        },
      ]
    : [
        // A plain question by default: nothing from the notes is sent.
        { kind: "none", id: "", label: "No context · just ask" },
        {
          kind: "workspace",
          id: workspaceId,
          label:
            "Workspace · " +
            (data.workspaces.find((w) => w.id === workspaceId)?.name || ""),
        },
        ...data.folders
          .filter((f) => f.workspace_id === workspaceId)
          .map((f) => ({
            kind: "folder",
            id: f.id,
            label: "Folder · " + f.name,
          })),
        ...data.notes
          .filter(
            (n) =>
              !n.trashed &&
              data.folders.some(
                (f) => f.id === n.folder_id && f.workspace_id === workspaceId,
              ),
          )
          .map((n) => ({ kind: "note", id: n.id, label: "Note · " + n.title })),
      ];
  const chosen = scopeOptions.findIndex(
    (s) => s.kind === initialScope?.kind && s.id === initialScope?.id,
  );
  $("#breadcrumb").textContent = $t("AI chat");
  $("#content").innerHTML =
    `<section class="page ai-page ${docked ? "docked" : ""}">${docked ? `<div class="page-heading ai-docked-heading"><h1><span class="chat-avatar" aria-hidden="true">✳</span>${esc(dockedTitle)}</h1><div class="ai-heading-actions"><button class="btn" id="ai-history-toggle" aria-expanded="false" aria-controls="ai-history">${$t("History")}</button><button class="btn" id="ai-settings">${$t("Providers")}</button></div></div>` : `<div class="eyebrow">${$t("ANSWERS, FROM ANYTHING YOU SHARE")}</div><div class="page-heading"><div><h1>${$t("Ask anything")}</h1><p class="subtitle">${$t("A plain question, or one about your notes. Attach photos, scans and documents to have them read.")}</p></div><button class="btn" id="ai-settings">${$t("Provider settings")}</button></div>`}<div class="ai-layout"><aside class="panel ai-history" id="ai-history" ${docked ? "hidden" : ""}><button class="btn" id="new-chat">${$t("+ New conversation")}</button><h3>${$t("Your conversations")}</h3><div id="chat-history"></div></aside><section class="panel ai-conversation"><div class="ai-controls"><label>${$t("Context")}<select id="chat-scope">${scopeOptions.map((s, i) => `<option value="${i}" ${i === Math.max(0, chosen) ? "selected" : ""}>${esc(s.label)}</option>`).join("")}</select></label><label>${$t("Model")}<select id="chat-provider">${providers.map((p) => `<option value="${p.id}">${esc(p.name)} · ${esc(p.model)}</option>`).join("") || `<option value="">${$t("No provider configured")}</option>`}</select></label></div><details class="ai-disclosure"><summary>${$t("What is sent to the model?")}</summary><p>${$t("Your question, the recent conversation, and any files you attach go to the selected provider. With a context, the permitted notes and records from it go too; with no context, none do. Context is capped; source coverage appears with each answer. Attached files are sent with that question only and are not stored. Replies may be inaccurate.")}</p></details><div id="chat-messages" class="chat-messages" aria-label="${$t("Conversation")}"><div class="empty"><h3>${$t("What would you like to understand?")}</h3><p>${$t("Ask a question, read a photo or a PDF, or pick a context to ask about your notes.")}</p></div></div><form id="chat-form" class="chat-composer"><label for="chat-question" class="sr-only">${$t("Your question")}</label><div id="chat-files" class="chat-files" hidden></div><div class="chat-input"><button type="button" class="chat-attach" id="chat-attach" title="${$t("Attach images or documents")}" aria-label="${$t("Attach images or documents")}" ${providers.length ? "" : "disabled"}>${CLIP}</button><input type="file" id="chat-file-input" multiple accept="${ACCEPT}" hidden><textarea id="chat-question" rows="1" maxlength="8000" placeholder="${docked ? $t("Ask about this note…") : $t("Ask anything…")}" required ${providers.length ? "" : "disabled"}></textarea><button class="btn primary chat-send" id="send-chat" title="${$t("Send (Enter)")}" aria-label="${$t("Send")}" ${providers.length ? "" : "disabled"}>↑</button></div><div class="chat-compose-actions"><span id="chat-status" role="status">${providers.length ? $t("Enter to send · Shift+Enter for a new line · drop images or documents here") : $t("Configure a provider to begin.")}</span><button class="btn" id="stop-chat" type="button" disabled>${$t("Stop")}</button></div></form></section></div></section>`;
  $("#ai-settings").onclick = configure;
  // Beside a note, past conversations open on demand.
  const historyToggle = $("#ai-history-toggle");
  if (historyToggle)
    historyToggle.onclick = () => {
      const panel = $("#ai-history");
      panel.hidden = !panel.hidden;
      historyToggle.setAttribute("aria-expanded", String(!panel.hidden));
    };
  // The question box grows with its text; Enter sends, Shift+Enter breaks a line.
  const questionBox = $("#chat-question");
  const fit = () => {
    questionBox.style.height = "auto";
    questionBox.style.height = Math.min(questionBox.scrollHeight, 220) + "px";
  };
  questionBox.addEventListener("input", fit);
  questionBox.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      $("#chat-form").requestSubmit();
    }
  });
  const scope = () => {
    const selected = scopeOptions[Number($("#chat-scope").value)];
    return selected && { kind: selected.kind, id: selected.id };
  };
  // What a conversation is about, by name, wherever it is.
  const scopeLabel = (scope) => {
    const find = (list, id) => (list || []).find((x) => x.id === id);
    if (scope.kind === "none") return "No context";
    if (scope.kind === "workspace")
      return `Workspace · ${find(data.workspaces, scope.id)?.name || "unavailable"}`;
    if (scope.kind === "folder")
      return `Folder · ${find(data.folders, scope.id)?.name || "unavailable"}`;
    const n = find(data.notes, scope.id) || find(data.shared_notes, scope.id);
    return `Note · ${n?.title || "unavailable"}`;
  };
  // The context select also offers a conversation's context from elsewhere.
  const scopeIndex = (scope) => {
    let i = scopeOptions.findIndex(
      (s) => s.kind === scope.kind && s.id === scope.id,
    );
    if (i < 0) {
      scopeOptions.push({ ...scope, label: scopeLabel(scope) });
      i = scopeOptions.length - 1;
      $("#chat-scope").insertAdjacentHTML(
        "beforeend",
        `<option value="${i}">${esc(scopeOptions[i].label)}</option>`,
      );
    }
    return i;
  };
  const ago = (seconds) => {
    const minutes = Math.round((Date.now() / 1000 - seconds) / 60);
    if (minutes < 1) return $t("just now");
    if (minutes < 60) return $t("{count} min ago", { count: minutes });
    if (minutes < 1440)
      return $t("{count} h ago", { count: Math.round(minutes / 60) });
    return new Date(seconds * 1000).toLocaleDateString();
  };
  const drawHistory = () => {
    // Every conversation, newest first; a chat beside a note shows that note's.
    const shown = docked
      ? history.filter(
          (c) =>
            c.scope.kind === initialScope.kind &&
            c.scope.id === initialScope.id,
        )
      : history;
    $("#chat-history").innerHTML =
      shown
        .map(
          (c) =>
            `<div class="chat-history-row ${chat?.id === c.id ? "current" : ""}"><button class="chat-history-item" data-chat="${c.id}" title="${esc(c.title)}"><span class="chat-history-title">${esc(c.title)}</span><small>${esc(scopeLabel(c.scope))} · ${ago(c.updated_at)}</small></button><button class="chat-delete" data-delete-chat="${c.id}" aria-label="Delete conversation “${esc(c.title)}”" title="${$t("Delete conversation")}">×</button></div>`,
        )
        .join("") ||
      `<p class="chat-history-empty">${$t("No conversations yet.")}</p>`;
    if (history.length)
      $("#chat-history").insertAdjacentHTML(
        "beforeend",
        `<button class="text-link chat-delete-all" id="delete-all-chats">${$t("Delete all conversations ({count})", { count: history.length })}</button>`,
      );
    // Deleting asks once more on the same button, then cannot be undone.
    const confirmed = (button, label) => {
      if (button.dataset.armed) return true;
      button.dataset.armed = "1";
      const original = button.textContent;
      button.textContent = label;
      button.classList.add("armed");
      setTimeout(() => {
        if (!button.isConnected) return;
        delete button.dataset.armed;
        button.textContent = original;
        button.classList.remove("armed");
      }, 4000);
      return false;
    };
    $$("[data-delete-chat]").forEach(
      (button) =>
        (button.onclick = async () => {
          if (running || !confirmed(button, $t("Delete?"))) return;
          const id = button.dataset.deleteChat;
          try {
            await api(`ai/chats/${id}`, "DELETE");
            history.splice(
              history.findIndex((c) => c.id === id),
              1,
            );
            if (chat?.id === id) reset();
            else drawHistory();
          } catch (err) {
            error(err);
          }
        }),
    );
    const all = $("#delete-all-chats");
    if (all)
      all.onclick = async () => {
        if (
          running ||
          !confirmed(all, $t("Delete every conversation? Click again"))
        )
          return;
        try {
          await api("ai/chats", "DELETE");
          history.length = 0;
          reset();
        } catch (err) {
          error(err);
        }
      };
    $$("[data-chat]").forEach(
      (button) =>
        (button.onclick = async () => {
          if (running) return;
          try {
            const loaded = await api(`ai/chats/${button.dataset.chat}`);
            if (epoch !== generation) return;
            chat = loaded;
            $("#chat-scope").value = String(scopeIndex(chat.scope));
            drawTurns();
            drawHistory();
          } catch (err) {
            error(err);
          }
        }),
    );
  };
  function sourcesHTML(sources) {
    return sources
      .map(
        (s, i) =>
          `<button class="source-chip" data-source="${esc(s.id)}" data-kind="${esc(s.kind)}" title="Open ${esc(s.title)}"><span class="source-number">S${i + 1}</span>${esc(s.title)}</button>`,
      )
      .join("");
  }
  /** Who answers: the assistant, with the provider and model. */
  function replyMeta(provider, model) {
    return `<div class="chat-meta"><span class="chat-avatar" aria-hidden="true">✳</span><strong>${esc(provider || $t("Assistant"))}</strong><span>${esc(model || "")}</span></div>`;
  }
  function bindSources(root) {
    $$("[data-source]", root).forEach(
      (button) =>
        (button.onclick = () => {
          (button.dataset.kind === "note"
            ? openNote(button.dataset.source)
            : openDatabase(button.dataset.source)
          ).catch(error);
        }),
    );
  }
  function imageHTML(url) {
    if (!/^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(url))
      return "";
    return `<a href="${esc(url)}" download="minerva-image"><img src="${esc(url)}" alt="${$t("Generated image")}" style="max-width:100%;max-height:600px"></a>`;
  }
  function drawTurns() {
    $("#chat-messages").innerHTML =
      chat?.turns
        .map(
          (t) =>
            `<article class="chat-turn"><p class="chat-question">${esc(t.question)}${filesHTML(t.files)}</p><div class="chat-reply">${replyMeta(t.provider, t.model)}<div class="chat-answer">${esc(t.answer)}</div><div class="chat-generated-images">${(t.images || []).map(imageHTML).join("")}</div><div class="chat-sources">${sourcesHTML(t.sources)}</div><small class="chat-usage">${usageLabel(t.usage)}</small></div></article>`,
        )
        .join("") ||
      `<div class="empty"><h3>${$t("A new conversation")}</h3><p>${$t("Ask a question to get started.")}</p></div>`;
    bindSources($("#chat-messages"));
    const scope = chat?.scope;
    $$(".chat-turn", $("#chat-messages")).forEach((turn, i) =>
      reply(turn, chat.turns[i].answer, chat.turns[i].question, scope),
    );
  }
  // Rendered reply blocks by Markdown, shared by every redraw.
  const rendered = new Map();
  async function renderBlocks(blocks) {
    const missing = [...new Set(blocks.filter((b) => !rendered.has(b)))];
    if (missing.length) {
      try {
        const { html } = await api("render", "POST", { blocks: missing });
        missing.forEach((b, i) => rendered.set(b, html[i]));
      } catch {
        // A server without batch rendering: one block at a time.
        for (const b of missing)
          rendered.set(b, (await api("render", "POST", { markdown: b })).html);
      }
    }
    return blocks.map((b) => rendered.get(b));
  }
  /** Shows a finished reply as note blocks, with actions to keep it. */
  async function reply(turn, markdown, question, scope) {
    const answer = $(".chat-answer", turn);
    const blocks = splitMarkdown(markdown).filter((b) => b.trim());
    let html;
    try {
      html = await renderBlocks(blocks);
    } catch {
      return; // Keep the plain text.
    }
    if (!answer.isConnected) return;
    const target =
      scope?.kind === "note" && canEditNote(scope.id) ? scope.id : null;
    answer.classList.remove("chat-pending");
    answer.classList.add("chat-blocks");
    answer.innerHTML = blocks
      .map(
        (_, i) =>
          `<div class="chat-block"><div class="chat-block-body">${html[i]}</div>${target ? `<button class="chat-block-add" data-add-block="${i}" title="${$t("Add this block to the end of the note")}" aria-label="${$t("Add this block to the note")}">${$t("＋ Note")}</button>` : ""}</div>`,
      )
      .join("");
    turn.querySelector(".chat-reply-actions")?.remove();
    if (!blocks.length) return;
    const actions = document.createElement("div");
    actions.className = "chat-reply-actions";
    actions.innerHTML = `<button class="btn" data-reply="copy">${$t("Copy")}</button><button class="btn" data-reply="note">${$t("New note")}</button>${target ? `<button class="btn primary" data-reply="add">${$t("Add all to note")}</button>` : ""}`;
    answer.after(actions);
    // Button feedback: a short confirmation, or the error.
    const run = async (button, label, work) => {
      button.disabled = true;
      const original = button.textContent;
      try {
        await work();
        button.textContent = label;
        setTimeout(() => (button.textContent = original), 1800);
      } catch (err) {
        error(err);
      } finally {
        button.disabled = false;
      }
    };
    $$("[data-add-block]", answer).forEach(
      (button) =>
        (button.onclick = () =>
          run(button, "✓ Added", () =>
            sendToNote(target, [blocks[Number(button.dataset.addBlock)]]),
          )),
    );
    $$("[data-reply]", actions).forEach(
      (button) =>
        (button.onclick = () =>
          ({
            copy: () => run(button, "✓ Copied", () => copy(markdown)),
            note: () =>
              run(button, "✓ Saved", () =>
                noteFromReply(markdown, question, scope),
              ),
            add: () => run(button, "✓ Added", () => sendToNote(target, blocks)),
          })[button.dataset.reply]()),
    );
  }
  async function copy(text) {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Clipboard API unavailable (for example without focus): select and copy.
      const area = document.createElement("textarea");
      area.value = text;
      area.style.position = "fixed";
      area.style.opacity = "0";
      document.body.append(area);
      area.select();
      const ok = document.execCommand("copy");
      area.remove();
      if (!ok) throw new Error($t("Copying is not allowed here."));
    }
  }
  function usageLabel(usage) {
    const input = usage?.input_tokens ?? usage?.prompt_tokens;
    const output = usage?.output_tokens ?? usage?.completion_tokens;
    return input != null || output != null
      ? $t("{input} input / {output} output tokens", {
          input: input ?? "?",
          output: output ?? "?",
        })
      : "Usage not reported";
  }
  function reset() {
    if (running) return;
    chat = null;
    const kind = scope()?.kind;
    $("#chat-question").placeholder = docked
      ? $t("Ask about this note…")
      : kind === "none"
        ? $t("Ask anything…")
        : $t("Ask a question about this context…");
    drawTurns();
    drawHistory();
    $("#chat-status").textContent = $t("New conversation · ready");
  }
  // Files for the next question: chosen with 📎, dropped on the composer, or
  // pasted (images).
  let files = [];
  const drawFiles = () => {
    const box = $("#chat-files");
    box.hidden = !files.length;
    box.innerHTML = files
      .map(
        (f, i) =>
          `<span class="chat-file">${fileIcon(f)}<span class="chat-file-name">${esc(f.name)}</span><small>${size(f.size)}</small><button type="button" data-unattach="${i}" aria-label="Remove ${esc(f.name)}">×</button></span>`,
      )
      .join("");
    $$("[data-unattach]", box).forEach(
      (b) =>
        (b.onclick = () => {
          files.splice(Number(b.dataset.unattach), 1);
          drawFiles();
        }),
    );
  };
  const attach = (list) => {
    for (const f of list) {
      const problem = attachmentProblem(f, files);
      if (problem) {
        $("#chat-status").textContent = problem;
        continue;
      }
      files.push(f);
    }
    drawFiles();
  };
  $("#chat-attach").onclick = () => $("#chat-file-input").click();
  $("#chat-file-input").onchange = (e) => {
    attach([...e.target.files]);
    e.target.value = "";
  };
  const composer = $("#chat-form");
  composer.addEventListener("dragover", (e) => {
    if (![...e.dataTransfer.types].includes("Files")) return;
    e.preventDefault();
    composer.classList.add("dropping");
  });
  composer.addEventListener("dragleave", (e) => {
    if (!composer.contains(e.relatedTarget))
      composer.classList.remove("dropping");
  });
  composer.addEventListener("drop", (e) => {
    composer.classList.remove("dropping");
    if (!e.dataTransfer.files.length) return;
    e.preventDefault();
    attach([...e.dataTransfer.files]);
  });
  questionBox.addEventListener("paste", (e) => {
    const pasted = [...e.clipboardData.files];
    if (!pasted.length) return;
    e.preventDefault();
    attach(pasted);
  });
  $("#new-chat").onclick = reset;
  $("#chat-scope").onchange = reset;
  $("#stop-chat").onclick = () => controller?.abort();
  $("#chat-form").onsubmit = async (e) => {
    e.preventDefault();
    if (running || !scope()) return;
    const question = $("#chat-question").value.trim();
    if (!question) return;
    const sending = files;
    let attachments;
    try {
      attachments = await Promise.all(sending.map(readAttachment));
    } catch (err) {
      $("#chat-status").textContent = err.message;
      return;
    }
    running = true;
    controller = new AbortController();
    [
      "send-chat",
      "chat-scope",
      "chat-provider",
      "new-chat",
      "chat-attach",
    ].forEach((id) => ($("#" + id).disabled = true));
    $("#stop-chat").disabled = false;
    $("#chat-status").textContent = $t("Preparing context…");
    const messages = $("#chat-messages");
    if (!chat?.turns.length) messages.innerHTML = "";
    const turn = document.createElement("article");
    turn.className = "chat-turn";
    const chosen = providers.find((p) => p.id === $("#chat-provider").value);
    turn.innerHTML = `<p class="chat-question">${esc(question)}${filesHTML(sending.map((f) => f.name))}</p><div class="chat-reply">${replyMeta(chosen?.name, chosen?.model)}<div class="chat-answer chat-pending"><span class="chat-dots" aria-hidden="true"><i></i><i></i><i></i></span><span class="chat-pending-text">${$t("Reading your notes…")}</span></div><div class="chat-sources"></div><small class="chat-usage"></small></div>`;
    messages.append(turn);
    turn.scrollIntoView({ block: "start", behavior: "smooth" });
    // The question is on its way: the box is free for the next one.
    $("#chat-question").value = "";
    files = [];
    drawFiles();
    fit();
    let complete = false;
    let stopShellPolling = () => {};
    // The reply is shown formatted while it streams, redrawn a few times a
    // second rather than on every token.
    let drawn = 0,
      drawing = null;
    const live = (text) => {
      const box = $(".chat-answer", turn);
      if (!drawn) box.classList.remove("chat-pending");
      if (drawing) return;
      drawing = setTimeout(
        async () => {
          drawing = null;
          const blocks = splitMarkdown(text).filter((b) => b.trim());
          try {
            const html = await renderBlocks(blocks);
            if (box.isConnected && !box.classList.contains("chat-blocks"))
              box.innerHTML = html
                .map((h) => `<div class="chat-block-body">${h}</div>`)
                .join("");
          } catch {
            box.textContent = text;
          }
        },
        drawn++ ? 350 : 0,
      );
    };
    try {
      if (!chat) {
        chat = await api("ai/chats", "POST", { scope: scope() });
        history.unshift(chat);
        drawHistory();
      }
      if (epoch !== generation || controller.signal.aborted)
        throw new DOMException("Stopped", "AbortError");
      if (localMode && chosen?.server_tools?.includes("minerva:shell")) {
        stopShellPolling = pollShell(api, esc, chat.id, controller.signal);
      }
      const response = await fetch(`/api/v1/ai/chats/${chat.id}/messages`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-Minerva-Client": "1",
        },
        body: JSON.stringify({
          message: question,
          attachments,
          provider_id: $("#chat-provider").value,
          revision: chat.turns.length,
        }),
        signal: controller.signal,
      });
      if (!response.ok) {
        const problem = await response.json();
        throw new Error(
          problem.error
            ? serverMessage(problem.error)
            : $t("Unable to start response"),
        );
      }
      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let pending = "";
      let answer = "",
        sources = [],
        usage = {};
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        pending += decoder.decode(value, { stream: true });
        let end;
        while ((end = pending.indexOf("\n")) >= 0) {
          const line = pending.slice(0, end);
          pending = pending.slice(end + 1);
          if (!line.trim()) continue;
          const event = JSON.parse(line);
          if (epoch !== generation) return;
          if (event.type === "context") {
            sources = event.sources;
            $(".chat-sources", turn).innerHTML = sourcesHTML(sources);
            bindSources(turn);
            $("#chat-status").textContent = event.truncated
              ? $t("Reading selected excerpts · context truncated")
              : "Reading " + sources.length + " sources";
            const pending = $(".chat-pending-text", turn);
            if (pending)
              pending.textContent = $tn(
                sources.length,
                "Reading {count} source…",
                "Reading {count} sources…",
              );
          }
          // Thinking models reason before they answer.
          if (event.type === "thinking" && !answer) {
            $("#chat-status").textContent = $t("Thinking…");
            const pending = $(".chat-pending-text", turn);
            if (pending) pending.textContent = $t("Thinking…");
          }
          if (event.type === "image") {
            let gallery = $(".chat-generated-images", turn);
            if (!gallery) {
              gallery = document.createElement("div");
              gallery.className = "chat-generated-images";
              $(".chat-answer", turn).after(gallery);
            }
            gallery.insertAdjacentHTML("beforeend", imageHTML(event.url));
          }
          if (event.type === "delta") {
            answer += event.text;
            live(answer);
            $("#chat-status").textContent = $t("Writing…");
          }
          if (event.type === "error")
            throw new Error(serverMessage(event.message));
          if (event.type === "done") {
            complete = true;
            usage = event.usage;
            $(".chat-usage", turn).textContent = usageLabel(usage);
            $("#chat-status").textContent = $t("Saved");
          }
        }
      }
      if (!complete)
        throw new Error(
          $t("Response interrupted. Partial output was not saved."),
        );
      chat = await api(`ai/chats/${chat.id}`);
      if (epoch !== generation) return;
      reply(turn, answer, question, chat.scope);
      Object.assign(
        history.find((c) => c.id === chat.id),
        chat,
      );
      drawHistory();
      $("#chat-question").value = "";
    } catch (err) {
      if (epoch === generation) {
        $("#chat-status").textContent =
          err.name === "AbortError"
            ? $t("Stopped · partial output was not saved")
            : err.message;
        turn.classList.add("chat-incomplete");
        // Not answered: the question goes back in the box to try again.
        if (!$("#chat-question").value) $("#chat-question").value = question;
        if (!files.length) {
          files = sending;
          drawFiles();
        }
      }
    } finally {
      stopShellPolling();
      if (epoch === generation) {
        running = false;
        controller = null;
        [
          "send-chat",
          "chat-scope",
          "chat-provider",
          "new-chat",
          "chat-attach",
        ].forEach((id) => ($("#" + id).disabled = false));
        $("#stop-chat").disabled = true;
      }
    }
  };
  drawHistory();
}

const CLIP =
  '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m21 11-8.5 8.5a5 5 0 0 1-7-7L14 4a3.5 3.5 0 0 1 5 5l-8.5 8.5a2 2 0 0 1-3-3L15 7"/></svg>';
/** Text documents are read here and sent as text. */
const TEXT_TYPES =
  /\.(txt|md|markdown|csv|tsv|json|xml|html?|ya?ml|log|ini|toml|js|ts|py|rs|go|java|c|h|cpp|cs|rb|php|sh|sql|css)$/i;
const ACCEPT =
  "image/png,image/jpeg,image/gif,image/webp,application/pdf,.pdf,text/*,.md,.csv,.tsv,.json,.xml,.yaml,.yml,.log,.toml,.ini,.js,.ts,.py,.rs,.go,.java,.c,.h,.cpp,.cs,.rb,.php,.sh,.sql,.css";
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const isText = (f) => f.type.startsWith("text/") || TEXT_TYPES.test(f.name);
const isPdf = (f) => f.type === "application/pdf" || /\.pdf$/i.test(f.name);
const size = (bytes) =>
  bytes < 1024 * 1024
    ? `${Math.max(1, Math.round(bytes / 1024))} KB`
    : `${(bytes / 1024 / 1024).toFixed(1)} MB`;
function fileIcon(f) {
  return `<span class="chat-file-icon" aria-hidden="true">${IMAGE_TYPES.includes(f.type) ? "🖼" : isPdf(f) ? "📕" : "📄"}</span>`;
}
/** Why `file` cannot be attached next to `files`, or null. */
export function attachmentProblem(file, files) {
  if (files.length >= 6) return "Attach up to 6 files to one question.";
  if (!IMAGE_TYPES.includes(file.type) && !isPdf(file) && !isText(file))
    return $t(
      "“{name}” cannot be read: attach images (PNG, JPEG, GIF, WebP), PDFs, or text documents.",
      { name: file.name },
    );
  if (file.size > 15 * 1024 * 1024)
    return $t("“{name}” is larger than 15 MB.", { name: file.name });
  const total = files.reduce((n, f) => n + f.size, file.size);
  if (total > 20 * 1024 * 1024)
    return "Attachments can be up to 20 MB together.";
  return null;
}
/** A file as the chat API takes it: text documents as text, others as base64. */
async function readAttachment(file) {
  if (isText(file)) return { name: file.name, text: await file.text() };
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 32768)
    binary += String.fromCharCode(...bytes.subarray(i, i + 32768));
  return { name: file.name, data: btoa(binary) };
}
function filesHTML(names) {
  if (!names?.length) return "";
  const esc = (t) =>
    t.replace(
      /[&<>"']/g,
      (c) =>
        ({
          "&": "&amp;",
          "<": "&lt;",
          ">": "&gt;",
          '"': "&quot;",
          "'": "&#39;",
        })[c],
    );
  return `<span class="chat-question-files">${names.map((n) => `<span class="chat-file small">📎 ${esc(n)}</span>`).join("")}</span>`;
}

async function configureShell(api, esc, error) {
  try {
    const config = await api("ai/shell/settings");
    const dialog = document.createElement("dialog");
    dialog.innerHTML = `<form><h2>${$t("Desktop Shell settings")}</h2><p>${$t("Commands run with your operating-system account privileges. The working folder is not a sandbox. Command output is sent to the selected provider.")}</p><label>${$t("Working folder")}<input name="directory" value="${esc(config.directory)}" placeholder="/absolute/path" required></label><label class="checkbox-label"><input type="checkbox" name="unattended" ${config.unattended ? "checked" : ""}>${$t("Allow unattended commands")}</label><p>${$t("Off by default: ask before each command. When enabled, the model may execute commands without asking while this chat runs.")}</p><p class="form-error" role="alert"></p><footer><button type="button" class="btn" data-cancel>${$t("Cancel")}</button><button type="submit" class="btn primary">${$t("Save")}</button></footer></form>`;
    document.body.append(dialog);
    dialog.showModal();
    dialog.onclose = () => dialog.remove();
    $("[data-cancel]", dialog).onclick = () => dialog.close();
    $("form", dialog).onsubmit = async (e) => {
      e.preventDefault();
      try {
        await api("ai/shell/settings", "PUT", {
          directory: $("[name=directory]", dialog).value,
          unattended: $("[name=unattended]", dialog).checked,
        });
        dialog.close();
      } catch (err) {
        $(".form-error", dialog).textContent = err.message;
      }
    };
  } catch (err) {
    error(err);
  }
}

function pollShell(api, esc, chatId, signal) {
  let stopped = false,
    timer,
    dialog;
  const stop = () => {
    if (stopped) return;
    stopped = true;
    clearTimeout(timer);
    dialog?.close();
    api(`ai/shell/cancel/${chatId}`, "POST", {}).catch(() => {});
  };
  signal.addEventListener("abort", stop, { once: true });
  const poll = async () => {
    try {
      const pending = await api(`ai/shell/pending/${chatId}`);
      for (const request of pending) {
        if (stopped) return;
        const approved = await new Promise((resolve) => {
          dialog = document.createElement("dialog");
          dialog.innerHTML = `<h2>${$t("Run this command?")}</h2><p>${$t("Working folder")}: ${esc(request.directory)}</p><pre style="white-space:pre-wrap">${esc(request.command)}</pre><p>${$t("Command output will be sent to the model.")}</p><footer><button class="btn" data-deny>${$t("Deny")}</button><button class="btn primary" data-allow>${$t("Run command")}</button></footer>`;
          document.body.append(dialog);
          let allowed = false;
          const current = dialog;
          current.onclose = () => {
            current.remove();
            dialog = null;
            resolve(allowed);
          };
          $("[data-deny]", current).onclick = () => current.close();
          $("[data-allow]", current).onclick = () => {
            allowed = true;
            current.close();
          };
          current.showModal();
        });
        if (!stopped)
          await api(`ai/shell/approval/${request.id}`, "POST", { approved });
      }
    } catch {
      /* A closed chat/session must never imply approval. */
    }
    if (!stopped) timer = setTimeout(poll, 750);
  };
  timer = setTimeout(poll, 750);
  return () => {
    signal.removeEventListener("abort", stop);
    stop();
  };
}
