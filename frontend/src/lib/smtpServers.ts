// Shared by the two email settings pages: the named SMTP servers and SMTP templates of
// upstream's App\Livewire\Concerns\ManagesSmtpServers, App\Support\SmtpPresets and
// email_provider_labels() (crates/coolify-jobs/src/notifications/email_settings.rs).

/** One entry of `smtpServerOptions()`: `active` is the server in use. */
export interface SmtpServerOption {
  value: string;
  label: string;
}

/** One entry of `SmtpPresets::PRESETS`. */
export interface SmtpPreset {
  value: string;
  label: string;
  host: string;
  port: number;
  encryption: string;
  username: string | null;
}

/** The names of the SMTP fields in a page's form (columns on one page, Livewire properties on the other). */
export interface SmtpFieldKeys {
  name: string;
  fromName: string;
  fromAddress: string;
  host: string;
  port: string;
  encryption: string;
  username: string;
  password: string;
  timeout: string;
  ehlo: string;
}

export type EmailProvider = 'none' | 'smtp' | 'resend';

/** `email_provider_labels()`, in the order that the provider selectors show them. */
export const emailProviderLabels: Record<EmailProvider, string> = {
  none: 'None',
  smtp: 'SMTP',
  resend: 'Resend',
};

export const emailProviderOptions = (Object.keys(emailProviderLabels) as EmailProvider[]).map((value) => ({
  value,
  label: emailProviderLabels[value],
}));
