<script lang="ts">
  // Port of upstream livewire/security/cloud-provider-token-form.blade.php with a
  // locked provider, in its `x-modal-input`.
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import Modal from './Modal.svelte';
  import { ApiError } from '../lib/api';
  import { cloudUi } from '../lib/cloudApi';
  import { failure } from '../lib/serverApi';
  import { notify } from '../lib/toast.svelte';

  let {
    provider,
    providerLabel,
    onclose,
    oncreated,
  }: { provider: string; providerLabel: string; onclose: () => void; oncreated: (token: { id: number; uuid: string }) => void } = $props();

  const consoleUrl = $derived(
    provider === 'hetzner'
      ? 'https://console.hetzner.com/projects'
      : provider === 'vultr'
        ? 'https://console.vultr.com/user/apiaccess/'
        : provider === 'hostinger'
          ? 'https://hpanel.hostinger.com/profile/api'
          : 'https://cloud.digitalocean.com/account/api/tokens',
  );

  let name = $state('');
  let token = $state('');
  let description = $state('');
  let error = $state<ApiError | null>(null);
  let busy = $state(false);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const saved = await cloudUi.addToken({ provider, token, name, description });
      notify.success(saved.message);
      oncreated(saved);
    } catch (err) {
      error = failure(err);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title="Add {providerLabel} Token" {onclose}>
  <div class="w-full">
    <form class="application-settings-form flex w-full flex-col gap-4" onsubmit={submit}>
      <input type="hidden" />

      <div class="rounded-lg border border-neutral-200 bg-neutral-50 p-3 text-[11px] leading-5 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-dim">
        Create the token in the
        <a href={consoleUrl} target="_blank" class="font-medium text-coollabs hover:underline dark:text-brand-light">
          <span>{providerLabel} console</span>{' '}
        </a>.
      </div>

      <div class="grid gap-4 lg:grid-cols-2">
        <FormInput required id="name" label="Token name" placeholder="Production {providerLabel} token" bind:value={name} error={error?.field('name')} />
        <FormInput required type="password" id="token" label="API token" placeholder="Paste the provider token" bind:value={token} error={error?.field('token')} />
        <div class="lg:col-span-2">
          <FormTextarea id="description" label="Description" rows={3} placeholder="Optional notes about where this token is used" bind:value={description} error={error?.field('description')} />
        </div>
      </div>

      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted" disabled={busy}>Validate and add</button>
      </div>
    </form>
  </div>
</Modal>
