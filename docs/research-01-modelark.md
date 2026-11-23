# ModelArk integration

The model catalogue lives in `config/seed-catalog.json`. It defines documented model IDs and capabilities. An entry is a candidate for use with an account. Successful generation also requires model activation and access in the selected region.

The native app stores API keys in macOS Keychain, performs provider requests in Rust and returns local asset identifiers to the React interface. Generation content is sent only as part of a requested workflow.

## API routes

| Operation | Route |
| --- | --- |
| List visible models | `GET /models` |
| Chat and assistant tools | `POST /chat/completions` |
| Image generation and editing | `POST /images/generations` |
| Create video task | `POST /contents/generations/tasks` |
| Read video task | `GET /contents/generations/tasks/{id}` |

Base URLs come from the selected region in the catalogue. Verify regional availability and the exact API contract when adding a provider or model. The BytePlus integration uses international ModelArk model IDs.

## Seedance 2.5 Flash

BytePlus lists `dreamina-seedance-2-5-flash-261115` for Seedance 2.5 Flash. Its documented output resolutions are 480p and 720p, with durations of 4–30 seconds or automatic length. It does not support the Seedance 2.5 draft workflow. UI controls and backend validation need to use those capabilities when the selected model changes. [Seedance guide](https://docs.byteplus.com/en/docs/ModelArk/seedance-2-5)

## Provider checks

Listing a model confirms catalogue visibility. A small chat test verifies a chat request. Image and video access is established by those respective generation requests. Explain access errors and let the user choose another enabled model or configure an endpoint ID where supported.

Record task IDs and preserve unknown outcomes to avoid duplicate billable submissions. Store returned media locally because provider result URLs can expire. Keep request logs limited to metadata and reported usage.

## Official references

- [Model list](https://docs.byteplus.com/en/docs/modelark/model-list)
- [Authentication and base URLs](https://docs.byteplus.com/en/docs/ModelArk/base-url-and-authentication)
- [Chat API](https://docs.byteplus.com/en/docs/modelark/chat-api)
- [Image generation API](https://docs.byteplus.com/en/docs/ModelArk/image-generation-api)
- [Create video task](https://docs.byteplus.com/en/docs/modelark/create-video-generation-task-api)
- [Read video task](https://docs.byteplus.com/en/docs/modelark/get-video-generation-task-api)
