// `Server\DockerRegistries\Login::PROVIDERS`.
export const REGISTRY_PROVIDERS: Record<string, { label: string; registry: string; username: string; placeholder: string; hint: string; host_pattern?: string }> = {
  "dockerhub": {
    "label": "Docker Hub",
    "registry": "docker.io",
    "username": "",
    "placeholder": "docker.io",
    "hint": "Use your Docker Hub username and a personal access token."
  },
  "ghcr": {
    "label": "GitHub Container Registry",
    "registry": "ghcr.io",
    "username": "",
    "placeholder": "ghcr.io",
    "hint": "Use your GitHub username and a personal access token (classic) with the read:packages scope."
  },
  "gitlab": {
    "label": "GitLab Container Registry",
    "registry": "registry.gitlab.com",
    "username": "",
    "placeholder": "registry.gitlab.com",
    "hint": "Use a deploy token or a personal access token with the read_registry scope."
  },
  "google": {
    "label": "Google Artifact Registry",
    "registry": "",
    "username": "_json_key",
    "placeholder": "europe-west1-docker.pkg.dev",
    "hint": "Paste the JSON key of a service account with the Artifact Registry Reader role.",
    "host_pattern": "/(^|\\.)gcr\\.io$|-docker\\.pkg\\.dev$/"
  },
  "azure": {
    "label": "Azure Container Registry",
    "registry": "",
    "username": "",
    "placeholder": "myregistry.azurecr.io",
    "hint": "Use a service principal ID and secret, or a repository-scoped token.",
    "host_pattern": "/\\.azurecr\\.(io|cn|us)$/"
  },
  "aws": {
    "label": "AWS ECR",
    "registry": "",
    "username": "AWS",
    "placeholder": "123456789012.dkr.ecr.eu-west-1.amazonaws.com",
    "hint": "Paste the output of aws ecr get-login-password. This token expires after 12 hours.",
    "host_pattern": "/^[0-9]{12}\\.dkr\\.ecr(-fips)?\\.[a-z0-9-]+\\.amazonaws\\.com(\\.cn)?$/"
  },
  "custom": {
    "label": "Other",
    "registry": "",
    "username": "",
    "placeholder": "registry.example.com",
    "hint": ""
  }
};
