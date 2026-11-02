# Roomer: the apps (marketing's side) built, reviewed with appfuzz, and
# deployed to the Roomer shell (the platform team's side).
#
#   make            check the tools, build the apps, review them
#   make serve      run the shell on http://127.0.0.1:8080
#   make deploy     build, review and deploy every app to the running shell
#
# Needs appfuzz, the roomer shell, and Rust with the wasm32-wasip2 target.

APPS := booking-grid pod-admin pod-stats quick-book
APPFUZZ ?= appfuzz
APPFUZZ_MIN := 0.2.0
# The installed shell, or one built here with `make platform`.
ROOMER ?= $(or $(shell command -v roomer 2>/dev/null),$(wildcard platform/target/release/roomer),roomer)
REVIEW_TIME ?= 5s
SERVER ?= http://127.0.0.1:8080
BY ?= agent:claude

WASM_DIR := apps/target/wasm32-wasip2/release
REVIEW_DIR := apps/target/review

.PHONY: all check tools kit-check apps manifests review deploy serve reset platform kit clean

all: check apps review

## The tools: appfuzz, roomer, Rust with the wasm32-wasip2 target.
tools:
	@command -v $(APPFUZZ) >/dev/null 2>&1 || { \
	  echo "appfuzz is not installed. Install it with:"; \
	  echo "  curl -fsSL https://appfuzz.dev/install.sh | sh"; exit 1; }
	@v=$$($(APPFUZZ) --version | awk '{print $$NF}'); \
	  if [ "$$(printf '%s\n%s\n' $(APPFUZZ_MIN) "$$v" | sort -V | head -n1)" != "$(APPFUZZ_MIN)" ]; then \
	    echo "appfuzz $$v is too old; Roomer needs $(APPFUZZ_MIN) or newer. Update it with:"; \
	    echo "  curl -fsSL https://appfuzz.dev/install.sh | sh"; exit 1; fi
	@command -v $(ROOMER) >/dev/null 2>&1 || [ -x "$(ROOMER)" ] || { \
	  echo "The roomer shell is not installed. Install it with:"; \
	  echo "  curl -fsSL https://raw.githubusercontent.com/StephanSchmidt/roomer-appos/main/install.sh | sh"; \
	  echo "or build it from platform/ with: make platform"; exit 1; }
	@command -v cargo >/dev/null 2>&1 || { echo "Rust is not installed: https://rustup.rs"; exit 1; }
	@rustup target list --installed 2>/dev/null | grep -qx wasm32-wasip2 || { \
	  echo "The wasm32-wasip2 target is missing. Add it with:"; \
	  echo "  rustup target add wasm32-wasip2"; exit 1; }

## The apps build against the platform kit only: apps/kit must be the
## platform's kit, and nothing in apps/ may refer to platform/.
kit-check:
	@diff -r -x target platform/kit apps/kit >/dev/null || { \
	  echo "apps/kit differs from platform/kit; copy the platform's kit with: make kit"; exit 1; }
	@if grep -rn --include='*.rs' --include='*.toml' --exclude-dir=target 'platform/' apps | grep -v 'appos:platform/'; then \
	  echo "apps/ refers to platform/ (above)"; exit 1; fi

check: tools kit-check

## Build every app as a WebAssembly component.
apps:
	cd apps && cargo build --release --target wasm32-wasip2

## The appfuzz manifest per app, written by the shell from the app's app.toml.
manifests: apps
	@for a in $(APPS); do \
	  mkdir -p $(REVIEW_DIR)/$$a; \
	  $(ROOMER) manifest --manifest apps/$$a/app.toml --wasm ../../wasm32-wasip2/release/$$(echo $$a | tr - _).wasm \
	    > $(REVIEW_DIR)/$$a/appfuzz.toml || exit 1; \
	done

## appfuzz review for every app: what the shell checks again on deploy.
## Exits non-zero if any app fails review.
review: manifests
	@status=0; for a in $(APPS); do \
	  echo "$$ (cd $(REVIEW_DIR)/$$a && $(APPFUZZ) review --time $(REVIEW_TIME))"; \
	  (cd $(REVIEW_DIR)/$$a && $(APPFUZZ) review --time $(REVIEW_TIME) > review.json); \
	  $(ROOMER) verdict $(REVIEW_DIR)/$$a/review.json --name $$a; \
	  [ $$? -ge 2 ] && status=2; \
	done; \
	echo "Drill down: cd $(REVIEW_DIR)/<app> && $(APPFUZZ) explain <run>"; \
	exit $$status

## Review, then deploy every app to the running shell. The shell reviews
## each version again and decides: fail is refused, warn goes live flagged.
deploy:
	-@$(MAKE) --no-print-directory review
	@for a in $(APPS); do \
	  $(ROOMER) deploy --manifest apps/$$a/app.toml --wasm $(WASM_DIR)/$$(echo $$a | tr - _).wasm --by "$(BY)" --server $(SERVER); \
	done; true

## Run the shell here (data in ./roomer-data).
serve:
	$(ROOMER) serve

## Forget all data, bookings and deployed apps.
reset:
	rm -rf roomer-data

## The platform team's side: build the roomer shell, with the apps built
## into it as the preinstalled ones.
platform: apps
	cd platform && cargo build --release
	@echo "Built platform/target/release/roomer"

## Copy the platform's kit (HOST-API.md, WIT, SDK) to the apps.
kit:
	rm -rf apps/kit && mkdir -p apps/kit && cp -r platform/kit/. apps/kit/

clean:
	rm -rf apps/target platform/target
