# Licensed under the MIT license
# <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
# option. This file may not be copied, modified, or distributed
# except according to those terms.

# Installs the latest version of the Apollo MCP Server.
# Specify a specific version to install with the $VERSION variable.

# Apollo MCP Server version defined in apollo-mcp-server's Cargo.toml
# Note: Change this line manually during the release steps.
$package_version = 'v1.4.0-experimental.0'

function Install-Binary($apollo_mcp_server_install_args) {
  $old_erroractionpreference = $ErrorActionPreference
  $ErrorActionPreference = 'stop'

  Initialize-Environment

  # If the VERSION env var is set, we use it instead
  # of the version defined in Apollo MCP Server's cargo.toml
  $download_version = if (Test-Path env:VERSION) {
    $Env:VERSION
  } else {
    $package_version
  }

  $exe = Download($download_version)

  Move-Item -Path $exe -Destination .

  Write-Host "Run `".\apollo-mcp-server.exe`" to start the server"

  $ErrorActionPreference = $old_erroractionpreference
}

function Download($version) {
  $binary_download_prefix = $env:APOLLO_ROVER_BINARY_DOWNLOAD_PREFIX
  if (-not $binary_download_prefix) {
    $binary_download_prefix = "https://github.com/apollographql/apollo-mcp-server/releases/download"
  }
  $url = "$binary_download_prefix/$version/apollo-mcp-server-$version-x86_64-pc-windows-msvc.tar.gz"

  # Remove credentials from the URL for logging
  $safe_url = $url -replace "https://[^@]+@", "https://"

  "Downloading Rover from $safe_url" | Out-Host
  $tmp = New-Temp-Dir
  $dir_path = "$tmp\apollo_mcp_server.tar.gz"
  $wc = New-Object Net.Webclient
  $wc.downloadFile($url, $dir_path)
  tar -xkf $dir_path -C "$tmp"
  return "$tmp\dist\apollo-mcp-server.exe"
}

function Initialize-Environment() {
  If (($PSVersionTable.PSVersion.Major) -lt 5) {
    Write-Error "PowerShell 5 or later is required to install Apollo MCP Server."
    Write-Error "Upgrade PowerShell: https://docs.microsoft.com/en-us/powershell/scripting/setup/installing-windows-powershell"
    break
  }

  # show notification to change execution policy:
  $allowedExecutionPolicy = @('Unrestricted', 'RemoteSigned', 'ByPass')
  If ((Get-ExecutionPolicy).ToString() -notin $allowedExecutionPolicy) {
    Write-Error "PowerShell requires an execution policy in [$($allowedExecutionPolicy -join ", ")] to run Apollo MCP Server."
    Write-Error "For example, to set the execution policy to 'RemoteSigned' please run :"
    Write-Error "'Set-ExecutionPolicy RemoteSigned -scope CurrentUser'"
    break
  }

  # GitHub requires TLS 1.2
  If ([System.Enum]::GetNames([System.Net.SecurityProtocolType]) -notcontains 'Tls12') {
    Write-Error "Installing Apollo MCP Server requires at least .NET Framework 4.5"
    Write-Error "Please download and install it first:"
    Write-Error "https://www.microsoft.com/net/download"
    break
  }

  If (-Not (Get-Command 'tar')) {
    Write-Error "The tar command is not installed on this machine. Please install tar before installing Apollo MCP Server"
    # don't abort if invoked with iex that would close the PS session
    If ($myinvocation.mycommand.commandtype -eq 'Script') { return } else { exit 1 }
  }
}

function New-Temp-Dir() {
  [CmdletBinding(SupportsShouldProcess)]
  param()
  $parent = [System.IO.Path]::GetTempPath()
  [string] $name = [System.Guid]::NewGuid()
  New-Item -ItemType Directory -Path (Join-Path $parent $name)
}

Install-Binary "$Args"
