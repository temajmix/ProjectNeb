#requires -Version 5.1
<#
.SYNOPSIS
  Apply D1 migrations, set Worker secrets from environment variables and deploy
  the ProjectNeb Pro Worker.

.DESCRIPTION
  Secrets are read from the process environment (never from files in the repo):
    DISCORD_CLIENT_ID, DISCORD_CLIENT_SECRET, LICENSE_PRIVATE_KEY,
    BOOSTY_WEBHOOK_SECRET
  Optional:
    DISCORD_REDIRECT_URI, LICENSE_PREVIOUS_PUBLIC_KEY

  Values are piped to `wrangler secret put` on stdin and are never printed.

.PARAMETER WhatIf
  Print the planned commands without executing them.

.PARAMETER DryRun
  Alias for -WhatIf (also accepted as --dry-run).

.EXAMPLE
  $env:DISCORD_CLIENT_ID = "..."
  $env:DISCORD_CLIENT_SECRET = "..."
  $env:LICENSE_PRIVATE_KEY = "..."
  $env:BOOSTY_WEBHOOK_SECRET = "..."
  ./scripts/deploy.ps1

.EXAMPLE
  ./scripts/deploy.ps1 -WhatIf
#>
[CmdletBinding(SupportsShouldProcess = $true, PositionalBinding = $false)]
param(
  [string]$Database = "projectneb-pro",
  [string]$WorkerName = "projectneb-pro",
  [string]$WranglerConfig = "wrangler.toml",
  [string]$WorkerUrl = "",
  [switch]$SkipMigrations,
  [switch]$SkipSecrets,
  [switch]$SkipDeploy,
  [switch]$DryRun,
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$RemainingArguments = @()
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# Accept the Unix-style spelling used by CI wrappers: --dry-run / --whatif.
foreach ($argument in $RemainingArguments) {
  switch ($argument.ToLowerInvariant()) {
    "--dry-run" { $DryRun = $true }
    "--whatif" { $DryRun = $true }
    default { throw "Unknown argument: $argument" }
  }
}

if ($DryRun) { $WhatIfPreference = $true }

$backendRoot = Split-Path -Parent $PSScriptRoot
$requiredSecrets = @(
  "DISCORD_CLIENT_ID",
  "DISCORD_CLIENT_SECRET",
  "LICENSE_PRIVATE_KEY",
  "BOOSTY_WEBHOOK_SECRET"
)
$optionalSecrets = @(
  "DISCORD_REDIRECT_URI",
  "LICENSE_PREVIOUS_PUBLIC_KEY"
)

function Invoke-Wrangler {
  param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)

  $output = & npx wrangler @Arguments 2>&1 | ForEach-Object { "$_" }
  $exitCode = $LASTEXITCODE
  $output | ForEach-Object { Write-Host $_ }
  if ($exitCode -ne 0) {
    throw "wrangler $($Arguments -join ' ') failed with exit code $exitCode"
  }
  return ($output -join "`n")
}

Push-Location $backendRoot
try {
  Write-Host "ProjectNeb Pro deploy"
  Write-Host "  backend : $backendRoot"
  Write-Host "  worker  : $WorkerName"
  Write-Host "  database: $Database"
  Write-Host ""

  if (-not $SkipSecrets -and -not $WhatIfPreference) {
    $missing = @($requiredSecrets | Where-Object {
      [string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($_))
    })
    if ($missing.Count -gt 0) {
      throw "Missing environment variable(s): $($missing -join ', ')"
    }
  }

  if ($SkipMigrations) {
    Write-Host "==> Skipping D1 migrations (-SkipMigrations)"
  } elseif ($PSCmdlet.ShouldProcess("D1 database '$Database' (remote)", "apply migrations")) {
    Write-Host "==> Applying D1 migrations"
    Invoke-Wrangler d1 migrations apply $Database --remote | Out-Null
  }

  if ($SkipSecrets) {
    Write-Host "==> Skipping Worker secrets (-SkipSecrets)"
  } else {
    foreach ($name in ($requiredSecrets + $optionalSecrets)) {
      $isRequired = $requiredSecrets -contains $name
      if (-not $isRequired -and
          [string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) {
        continue
      }
      if ($PSCmdlet.ShouldProcess("Worker secret '$name'", "set")) {
        Write-Host "==> Setting secret $name"
        $value = [Environment]::GetEnvironmentVariable($name)
        $value | & npx wrangler secret put $name
        if ($LASTEXITCODE -ne 0) {
          throw "wrangler secret put $name failed with exit code $LASTEXITCODE"
        }
      }
    }
  }

  $deployedUrl = $null
  if ($SkipDeploy) {
    Write-Host "==> Skipping deploy (-SkipDeploy)"
  } elseif ($PSCmdlet.ShouldProcess("Worker '$WorkerName'", "deploy")) {
    Write-Host "==> Deploying Worker"
    $deployOutput = Invoke-Wrangler deploy --config $WranglerConfig
    $match = [regex]::Match($deployOutput, "https://[A-Za-z0-9._-]+\.workers\.dev")
    if ($match.Success) { $deployedUrl = $match.Value }
  }

  $finalUrl = if ($WorkerUrl) {
    $WorkerUrl
  } elseif ($deployedUrl) {
    $deployedUrl
  } elseif ($WhatIfPreference -or $SkipDeploy) {
    "(not deployed)"
  } else {
    "(could not read the URL from wrangler output; pass -WorkerUrl)"
  }

  Write-Host ""
  Write-Host "Worker URL: $finalUrl"
  if ($WhatIfPreference) {
    Write-Host "Dry run complete; no migrations, secrets or deployments were changed."
  }
}
finally {
  Pop-Location
}
