<script lang="ts">
  // Port of upstream livewire/subscription/{index,show,pricing-plans,actions}.blade.php. Outside
  // Coolify Cloud both pages redirect to the dashboard, as their `mount` does.
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Modal from '../components/Modal.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { ApiError, request } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  type Data = {
    cloud: boolean;
    is_admin: boolean;
    has_subscription: boolean;
    active: boolean;
    provider: string | null;
    team_name: string;
    plan: string | null;
    billing_interval: string;
    cancel_at_period_end: boolean;
    server_limits: number;
    active_servers: number;
    servers: number;
    overflow: boolean;
    can_delete_account: boolean;
    min_server_limit: number;
    max_server_limit: number;
    contact: string;
  };
  type Preview = {
    due_now: number;
    recurring_subtotal: number;
    recurring_tax: number;
    recurring_total: number;
    unit_price: number;
    tax_description: string | null;
    quantity: number;
    currency: string;
    billing_interval: string;
  };
  type Answer = { error?: string; message?: string; redirect?: string; invalid_password?: string; delete_account?: boolean };

  let { page: which }: { page: 'show' | 'index' } = $props();

  let data = $state<Data | null>(null);
  let loading = $state(true);
  let isUnpaid = $state(false);
  let isCancelled = $state(false);
  let selectedPlan = $state<'monthly' | 'yearly'>('monthly');

  async function load() {
    const d = await request<Data>('GET', '/ui/subscription');
    if (!d.cloud) return router.go('/', true);
    if (which === 'show') {
      if (!d.is_admin) return router.go('/', true);
      if (!d.has_subscription) return router.go('/subscription/new', true);
      document.title = 'Subscription | coolify-rs';
      data = d;
      qty = d.server_limits;
      loadRefund();
      return;
    }
    if (d.has_subscription && d.active) return router.go('/subscription', true);
    document.title = 'Subscribe | coolify-rs';
    data = d;
    loading = d.has_subscription;
    if (d.has_subscription && d.is_admin) {
      const s = await request<{ redirect: boolean; is_unpaid: boolean; is_cancelled: boolean; error: string | null }>('POST', '/ui/subscription/status', {});
      if (s.redirect) return router.go('/subscription');
      isUnpaid = s.is_unpaid;
      isCancelled = s.is_cancelled;
      if (s.error) notify.error(s.error);
      loading = false;
    }
  }
  load();

  async function portal() {
    const r = await request<{ url: string | null }>('POST', '/ui/subscription/portal', {});
    if (r.url) window.location.assign(r.url);
  }

  async function subscribe(type: string) {
    const r = await request<{ url?: string; error?: string }>('POST', '/ui/subscription/subscribe', { type });
    if (r.url) window.location.assign(r.url);
    else if (r.error) notify.error(r.error);
  }

  // Plan and billing.
  let qty = $state(2);
  let showModal = $state(false);
  let previewing = $state(false);
  let preview = $state<Preview | null>(null);
  let confirming = $state(false);
  let refundLoading = $state(true);
  let eligible = $state(false);
  let refundDays = $state(0);
  let refundUsed = $state(false);
  let nextBilling = $state<string | null>(null);
  let modal = $state<'period-end' | 'cancel-now' | 'refund' | null>(null);

  const hasChanged = $derived(!!data && qty !== data.server_limits);
  const isReduction = $derived(!!data && qty < data.active_servers);
  const fmt = (cents?: number) => {
    if (!preview || cents === undefined) return '';
    return preview.currency === 'USD' ? `$${(cents / 100).toFixed(2)}` : `${(cents / 100).toFixed(2)} ${preview.currency}`;
  };

  async function loadRefund() {
    const r = await request<{ eligible: boolean; days_remaining: number; already_used: boolean; next_billing_date: string | null }>('GET', '/ui/subscription/refund-eligibility');
    eligible = r.eligible;
    refundDays = r.days_remaining;
    refundUsed = r.already_used;
    nextBilling = r.next_billing_date;
    refundLoading = false;
  }

  async function fetchPreview() {
    if (!data || qty < 2 || qty > 100 || qty === data.server_limits) return;
    previewing = true;
    preview = null;
    preview = (await request<{ preview: Preview | null }>('POST', '/ui/subscription/preview', { quantity: qty })).preview;
    previewing = false;
  }

  function closeAdjust() {
    showModal = false;
    qty = data?.server_limits ?? qty;
    preview = null;
  }

  async function updateQuantity() {
    const r = await request<Answer & { server_limits?: number; quantity?: number }>('POST', '/ui/subscription/quantity', { quantity: qty });
    confirming = false;
    if (r.error) {
      notify.error(r.error);
      if (r.quantity !== undefined) qty = r.quantity;
      return;
    }
    if (r.message && data && r.server_limits !== undefined) {
      data.server_limits = r.server_limits;
      preview = null;
      showModal = false;
      notify.success(r.message);
    }
  }

  async function answered(path: string, body: unknown): Promise<string | void> {
    try {
      const r = await request<Answer>('POST', path, body);
      if (r.invalid_password) return r.invalid_password;
      modal = null;
      if (r.delete_account) {
        await request('POST', '/ui/profile/delete', body);
        window.location.assign('/login');
        return;
      }
      if (r.error) {
        notify.error(r.error);
        return;
      }
      if (r.message) notify.success(r.message);
      if (r.redirect) router.go(r.redirect);
      else await load();
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }

  const deleteAccountBox = {
    id: 'deleteAccount',
    label:
      'Delete my account. Servers, projects, and Git sources of teams where I am the only member are removed from coolify-rs. Nothing is stopped or deleted on your servers.',
  };
</script>

{#if data && which === 'index'}
  <div class="application-settings-form w-full max-w-none">
    <div class="mb-6">
      <h1>Subscription</h1>
      <p class="subtitle">Choose a plan for Coolify Cloud</p>
    </div>
    {#if data.is_admin}
      {#if loading}
        <div class="text-sm text-neutral-500">Loading your subscription status</div>
      {:else if isUnpaid}
        <SettingsSection title="Payment failed" description="Your latest Coolify Cloud payment could not be processed.">
          <Callout type="danger" title="Subscription payment is past due">Update the payment method or settle the outstanding invoice in the billing portal.</Callout>
          <div class="mt-4">
            <button type="button" class="button button-highlighted" onclick={portal}>Open billing portal</button>
          </div>
        </SettingsSection>
      {:else}
        {#if isCancelled || !data.has_subscription}
          <Callout type="warning" title="No active subscription" class="mb-6">Choose a plan to continue using Coolify Cloud.</Callout>
        {/if}
        <div class="w-full">
          <SettingsSection title="Pay as you go" description="Dynamic pricing based on the number of servers connected to your team.">
            {#snippet actions()}
              <div class="flex h-8 items-center rounded-lg border border-neutral-200 bg-neutral-100 p-0.5 dark:border-white/[0.08] dark:bg-white/[0.035]">
                {#each ['monthly', 'yearly'] as const as plan (plan)}
                  <button
                    type="button"
                    onclick={() => (selectedPlan = plan)}
                    class="app-tab h-6! px-2.5! {selectedPlan === plan ? 'bg-coollabs/10 text-coollabs ring-1 ring-coollabs/25 dark:bg-brand/15 dark:text-brand-light dark:ring-brand-light/25' : ''}"
                  >
                    {plan === 'monthly' ? 'Monthly' : 'Yearly'}
                  </button>
                {/each}
              </div>
            {/snippet}
            <div class="grid gap-6 lg:grid-cols-[minmax(0,.8fr)_minmax(0,1.2fr)]">
              <div class="rounded-[10px] border border-neutral-200 bg-neutral-50 p-4 dark:border-white/[0.08] dark:bg-white/[0.05]">
                <p class="text-[11px] font-medium text-neutral-500 dark:text-fg-faint">Base price</p>
                <div class="mt-2 flex items-end gap-1.5">
                  <span class="text-2xl font-semibold tracking-tight">{selectedPlan === 'monthly' ? '$5' : '$4'}</span>
                  <span class="pb-0.5 text-[12px] text-neutral-500 dark:text-fg-dim">per month</span>
                </div>
                <p class="mt-2 text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">
                  {selectedPlan === 'monthly' ? '$3 per additional server, billed monthly.' : '$2.70 per additional server, billed annually.'}
                </p>
                <div class="mt-4">
                  <button type="button" class="button button-highlighted w-full justify-center" onclick={() => subscribe(`dynamic-${selectedPlan}`)}>
                    Subscribe {selectedPlan}
                  </button>
                </div>
              </div>
              <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
                {#each ['Connect unlimited servers', 'Deploy unlimited resources per server', 'Transactional email notifications', 'Support by email', 'All upcoming platform features'] as feature (feature)}
                  <div class="flex min-h-10 items-center gap-2.5 py-2 text-[12px]">
                    <Reicon name="check-circle" class="size-4 shrink-0 text-emerald-500" />
                    <span>{feature}</span>
                  </div>
                {/each}
              </div>
            </div>
            <p class="mt-5 text-[11px] leading-5 text-neutral-500 dark:text-fg-faint">
              Bring your own servers from any cloud provider or supported Linux machine. Prices exclude applicable taxes.
            </p>
          </SettingsSection>
        </div>
      {/if}
    {:else}
      <SettingsSection title="Subscription" description="Only team administrators can manage billing and plan limits.">
        <Callout type="danger" title="Insufficient Permissions">
          You are not an admin so you cannot manage your Team's subscription. If this does not make sense, please
          <a class="underline cursor-pointer dark:text-white" href={data.contact} target="_blank">contact us</a>.
        </Callout>
      </SettingsSection>
    {/if}
  </div>
{:else if data}
  <div class="application-settings-form w-full max-w-none">
    <div class="mb-6">
      <h1>Subscription</h1>
      <p class="subtitle">Plan and billing for Coolify Cloud</p>
    </div>
    <div class="application-settings-workspace flex flex-col gap-6">
      {#if data.provider === 'stripe'}
        <SettingsSection title="Plan and billing">
          <div class="flex flex-col gap-4">
            <div class="space-y-3">
              <div class="flex flex-wrap items-center gap-3">
                <h2 class="text-lg! font-semibold!">{data.plan === 'dynamic' ? 'Pay-as-you-go' : data.plan}</h2>
                <span class="text-sm text-neutral-500">{data.billing_interval === 'yearly' ? 'Yearly' : 'Monthly'} billing</span>
                {#if data.cancel_at_period_end}
                  <StatusBadge type="warning" label="Cancelling at end of period" />
                {:else}
                  <StatusBadge type="success" label="Active" />
                {/if}
              </div>
              <div class="text-sm flex items-center gap-2 flex-wrap">
                <span>
                  <span class="text-neutral-500">Active servers:</span>
                  <span class="font-medium {data.overflow ? 'text-red-500' : 'dark:text-white'}">{data.active_servers}</span>
                  <span class="text-neutral-500">/</span>
                  <span class="font-medium dark:text-white">{data.server_limits}</span>
                  <span class="text-neutral-500">paid</span>
                </span>
                {#if refundLoading}
                  <span class="text-neutral-500">&middot;</span>
                  <span class="text-neutral-500">Loading...</span>
                {:else if nextBilling}
                  <span class="text-neutral-500">&middot;</span>
                  <span class="text-neutral-500">
                    {data.cancel_at_period_end ? 'Cancels on' : 'Next billing'} <span class="dark:text-white font-medium">{nextBilling}</span>
                  </span>
                {/if}
              </div>
            </div>
            {#if data.overflow}
              <Callout type="danger" title="Server limit exceeded" class="mt-4">
                You must delete {data.servers - data.server_limits} servers or upgrade your subscription. Excess servers will be deactivated.
              </Callout>
            {/if}
            <div class="flex flex-wrap items-center gap-2">
              <button type="button" class="button button-highlighted" onclick={() => (showModal = true)}>Adjust server limit</button>
              <button type="button" class="button gap-2" onclick={portal}>
                <Reicon name="subscription" class="size-3.5" />
                Open billing portal
              </button>
            </div>
          </div>
        </SettingsSection>

        <SettingsSection title="Manage subscription">
          <section aria-labelledby="subscription-cancellation" class="flex flex-col gap-3">
            <div class="space-y-1">
              <h3 id="subscription-cancellation" class="text-sm! font-medium!">Cancel subscription</h3>
              <p class="text-sm text-neutral-500">Keep access until the end of your billing period, or cancel immediately.</p>
            </div>
            <div class="flex flex-wrap items-center gap-2">
              {#if data.cancel_at_period_end}
                <button type="button" class="button" onclick={() => answered('/ui/subscription/resume', {})}>Resume Subscription</button>
              {:else}
                <button type="button" class="button" onclick={() => (modal = 'period-end')}>Cancel at Period End</button>
                <button type="button" class="button button-error" onclick={() => (modal = 'cancel-now')}>Cancel Immediately</button>
              {/if}
            </div>
            {#if data.cancel_at_period_end}
              <p class="text-sm text-neutral-500">Your subscription is set to cancel at the end of the billing period.</p>
            {/if}
          </section>

          <section aria-labelledby="subscription-refund" class="mt-5 flex flex-col gap-3 border-t border-neutral-200 pt-5 dark:border-white/[0.08]">
            <h3 id="subscription-refund" class="text-sm! font-medium!">Refund</h3>
            {#if !refundLoading && eligible}
              <div class="flex flex-wrap items-center gap-2">
                <button type="button" class="button button-error" onclick={() => (modal = 'refund')}>Request Full Refund</button>
              </div>
            {/if}
            <div class="text-sm text-neutral-500">
              {#if refundLoading}
                Checking refund eligibility...
              {:else if eligible}
                Eligible for a full refund &mdash; <strong class="dark:text-warning">{refundDays}</strong> days remaining.
              {:else if refundUsed}
                Refund already processed. Each team is eligible for one refund only.
              {:else}
                Not eligible for a refund.
              {/if}
            </div>
          </section>
        </SettingsSection>

        <div class="text-sm text-neutral-500">
          Need help? <a class="underline dark:text-white" href={data.contact} target="_blank">Contact us.</a>
        </div>
      {/if}
    </div>
  </div>

  {#if showModal}
    <Modal title="Adjust server limit" onclose={closeAdjust}>
      <div class="space-y-4">
        <div>
          <label class="mb-1.5 block" for="paid-servers">Paid servers</label>
          <div class="flex items-center gap-3">
            <input
              id="paid-servers"
              type="number"
              min={data.min_server_limit}
              max={data.max_server_limit}
              step="1"
              bind:value={qty}
              oninput={() => (preview = null)}
              onchange={() => (qty = Math.min(data?.max_server_limit ?? 100, Math.max(data?.min_server_limit ?? 2, qty || (data?.min_server_limit ?? 2))))}
              class="h-8! w-24 rounded-lg! border-neutral-200! bg-white! px-2! py-0! text-center! text-[13px]!"
            />
            <button type="button" class="button button-highlighted" disabled={!hasChanged || previewing} onclick={fetchPreview}>Calculate Price</button>
          </div>
        </div>
        {#if previewing}
          <div class="text-sm text-neutral-500">Loading price preview...</div>
        {:else if preview}
          <div class="space-y-4">
            <div>
              <div class="text-xs font-bold text-neutral-500 uppercase tracking-wide pb-1.5">Due now</div>
              <div class="flex justify-between gap-6 text-sm font-bold">
                <span class="dark:text-white">Prorated charge</span>
                <span class="dark:text-warning">{fmt(preview.due_now)}</span>
              </div>
              <p class="text-xs text-neutral-500 pt-1">Charged immediately to your payment method.</p>
            </div>
            <div>
              <div class="text-xs font-bold text-neutral-500 uppercase tracking-wide pb-1.5">
                Next billing cycle
                {#if nextBilling}<span class="normal-case font-normal">&middot; {nextBilling}</span>{/if}
              </div>
              <div class="space-y-1.5">
                <div class="flex justify-between gap-6 text-sm">
                  <span class="text-neutral-500">{preview.quantity} servers × {fmt(preview.unit_price)}</span>
                  <span class="dark:text-white">{fmt(preview.recurring_subtotal)}</span>
                </div>
                {#if preview.tax_description}
                  <div class="flex justify-between gap-6 text-sm">
                    <span class="text-neutral-500">{preview.tax_description}</span>
                    <span class="dark:text-white">{fmt(preview.recurring_tax)}</span>
                  </div>
                {/if}
                <div class="flex justify-between gap-6 text-sm font-bold pt-1.5 border-t dark:border-coolgray-400">
                  <span class="dark:text-white">Total / {preview.billing_interval === 'year' ? 'year' : 'month'}</span>
                  <span class="dark:text-white">{fmt(preview.recurring_total)}</span>
                </div>
              </div>
            </div>
            <button type="button" class="button w-full" onclick={() => (confirming = true)}>Update Server Limit</button>
          </div>
        {/if}
        {#if isReduction}
          <Callout type="danger" title="Warning">Reducing below your active server count will deactivate excess servers.</Callout>
        {/if}
      </div>
    </Modal>
  {/if}

  {#if confirming}
    <ConfirmModal
      title="Confirm Server Limit Update"
      actions={['Your server limit will be updated immediately.', 'The prorated amount will be invoiced and charged now.']}
      warningMessage="This will update your subscription and charge the prorated amount to your payment method."
      confirmWithText={false}
      step2ButtonText="Confirm & Pay"
      onconfirm={async () => updateQuantity()}
      onclose={() => (confirming = false)}
    />
  {:else if modal === 'period-end'}
    <ConfirmModal
      title="Cancel at End of Billing Period?"
      actions={['Your subscription will remain active until the end of the current billing period.', 'No further charges will be made after the current period.', 'You can resubscribe at any time.']}
      confirmationText={data.team_name}
      confirmationLabel="Enter your team name to confirm"
      shortConfirmationLabel="Team Name"
      step2ButtonText="Confirm Cancellation"
      confirmWithPassword={!session.info?.user.skip_password_confirmation}
      onconfirm={(password) => answered('/ui/subscription/cancel-at-period-end', { password })}
      onclose={() => (modal = null)}
    />
  {:else if modal === 'cancel-now'}
    <ConfirmModal
      title="Cancel Immediately?"
      checkboxes={[
        ...(eligible ? [{ id: 'refundLatestPayment', label: `Refund my latest payment (eligible for ${refundDays} more days).`, defaultWarning: 'No refund will be issued for the remaining period.' }] : []),
        ...(data.can_delete_account ? [deleteAccountBox] : []),
      ]}
      actions={eligible
        ? ['Your subscription will be cancelled immediately.', 'All servers will be deactivated.']
        : ['Your subscription will be cancelled immediately.', 'All servers will be deactivated.', 'No refund will be issued for the remaining period.']}
      confirmationText={data.team_name}
      confirmationLabel="Enter your team name to confirm"
      shortConfirmationLabel="Team Name"
      step2ButtonText="Permanently Cancel"
      confirmWithPassword={!session.info?.user.skip_password_confirmation}
      onconfirm={(password, selected) => answered('/ui/subscription/cancel-now', { password, selected })}
      onclose={() => (modal = null)}
    />
  {:else if modal === 'refund'}
    <ConfirmModal
      title="Request Full Refund?"
      actions={['Your latest payment will be fully refunded.', 'Your subscription will be cancelled immediately.', 'All servers will be deactivated.']}
      confirmationText={data.team_name}
      confirmationLabel="Enter your team name to confirm"
      shortConfirmationLabel="Team Name"
      step2ButtonText="Confirm Refund & Cancel"
      confirmWithPassword={!session.info?.user.skip_password_confirmation}
      onconfirm={(password) => answered('/ui/subscription/refund', { password })}
      onclose={() => (modal = null)}
    />
  {/if}
{/if}
