// <tinox-counter> -- a real, self-registering custom element, loaded via
// Component::jsComponent's dynamic import() (see CounterDemoApp.tnx).
// Deliberately the SAME shape a real Vaadin @Tag/@JsModule component's
// own JS side would take: define the element, react to attribute
// changes via the standard Custom Elements API, report interaction back
// through the one method Tinox-UI's runtime attaches to the element
// itself (`this.tinoxSendEvent`) -- nothing Tinox-specific beyond that.
//
// Its own click count lives entirely in `this._localClicks`, on the
// element instance -- NOT in a window-global (unlike the older
// Component::html()-based logs-viewer.js/exec-viewer.js pattern this
// widget replaces the general mechanism for), because the element
// itself is never torn down by an ordinary Tinox-UI re-render anymore
// (see Assets.tnx's applyUpdate: a JsComponent slot only ever gets
// attribute patches). connectedCallback runs exactly once per element
// lifetime, proving that.
class TinoxCounter extends HTMLElement {
  static get observedAttributes() {
    return ["label"];
  }

  constructor() {
    super();
    this._localClicks = 0;
  }

  connectedCallback() {
    this._labelEl = document.createElement("div");
    this._labelEl.textContent = this.getAttribute("label") || "";

    this._localEl = document.createElement("div");
    this._renderLocal();

    var btn = document.createElement("button");
    btn.type = "button";
    btn.textContent = "Increment locally + report";
    var self = this;
    btn.addEventListener("click", function () {
      self._localClicks += 1;
      self._renderLocal();
      if (typeof self.tinoxSendEvent === "function") {
        self.tinoxSendEvent(String(self._localClicks));
      }
    });

    this.appendChild(this._labelEl);
    this.appendChild(this._localEl);
    this.appendChild(btn);
  }

  attributeChangedCallback(name, oldValue, newValue) {
    if (name === "label" && this._labelEl) {
      this._labelEl.textContent = newValue || "";
    }
  }

  _renderLocal() {
    if (this._localEl) {
      this._localEl.textContent = "local clicks: " + this._localClicks;
    }
  }
}

customElements.define("tinox-counter", TinoxCounter);
