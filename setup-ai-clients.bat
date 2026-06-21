@echo off
setlocal
set "SCRIPT_PATH=%~f0"
powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand "JABFAHIAcgBvAHIAQQBjAHQAaQBvAG4AUAByAGUAZgBlAHIAZQBuAGMAZQA9ACcAUwB0AG8AcAAnADsAdAByAHkAewAkAG0APQAnACMAIABQAE8AVwBFAFIAUwBIAEUATABMAF8AUABBAFkATABPAEEARAAnADsAJAB0AD0AWwBJAE8ALgBGAGkAbABlAF0AOgA6AFIAZQBhAGQAQQBsAGwAVABlAHgAdAAoACQAZQBuAHYAOgBTAEMAUgBJAFAAVABfAFAAQQBUAEgALABbAFQAZQB4AHQALgBFAG4AYwBvAGQAaQBuAGcAXQA6ADoAVQBUAEYAOAApADsAJABpAD0AJAB0AC4ASQBuAGQAZQB4AE8AZgAoACQAbQApADsAaQBmACgAJABpACAALQBsAHQAIAAwACkAewB0AGgAcgBvAHcAIAAnAFAAbwB3AGUAcgBTAGgAZQBsAGwAIABwAGEAeQBsAG8AYQBkACAAdwBhAHMAIABuAG8AdAAgAGYAbwB1AG4AZAAuACcAfQA7AGkAZQB4ACAAJAB0AC4AUwB1AGIAcwB0AHIAaQBuAGcAKAAkAGkAKwAkAG0ALgBMAGUAbgBnAHQAaAApAH0AYwBhAHQAYwBoAHsAJABsAD0ASgBvAGkAbgAtAFAAYQB0AGgAIAAoAFMAcABsAGkAdAAtAFAAYQB0AGgAIAAtAFAAYQByAGUAbgB0ACAAJABlAG4AdgA6AFMAQwBSAEkAUABUAF8AUABBAFQASAApACAAJwBzAGUAdAB1AHAALQBhAGkALQBjAGwAaQBlAG4AdABzAC0AbABhAHMAdAAtAGUAcgByAG8AcgAuAGwAbwBnACcAOwBTAGUAdAAtAEMAbwBuAHQAZQBuAHQAIAAtAEwAaQB0AGUAcgBhAGwAUABhAHQAaAAgACQAbAAgAC0AVgBhAGwAdQBlACAAJABfAC4ARQB4AGMAZQBwAHQAaQBvAG4ALgBUAG8AUwB0AHIAaQBuAGcAKAApACAALQBFAG4AYwBvAGQAaQBuAGcAIABVAFQARgA4ADsAVwByAGkAdABlAC0ASABvAHMAdAAgACcAJwA7AFcAcgBpAHQAZQAtAEgAbwBzAHQAIAAnAFsARQBSAFIATwBSAF0AIABDAGwAaQBlAG4AdAAgAHMAZQB0AHUAcAAgAGYAYQBpAGwAZQBkADoAJwAgAC0ARgBvAHIAZQBnAHIAbwB1AG4AZABDAG8AbABvAHIAIABSAGUAZAA7AFcAcgBpAHQAZQAtAEgAbwBzAHQAIAAkAF8ALgBFAHgAYwBlAHAAdABpAG8AbgAuAE0AZQBzAHMAYQBnAGUAIAAtAEYAbwByAGUAZwByAG8AdQBuAGQAQwBvAGwAbwByACAAUgBlAGQAOwBXAHIAaQB0AGUALQBIAG8AcwB0ACAAJwAnADsAVwByAGkAdABlAC0ASABvAHMAdAAgACgAJwBFAHIAcgBvAHIAIABsAG8AZwA6ACAAJwArACQAbAApACAALQBGAG8AcgBlAGcAcgBvAHUAbgBkAEMAbwBsAG8AcgAgAFkAZQBsAGwAbwB3ADsAZQB4AGkAdAAgADEAfQA="
set "EXITCODE=%ERRORLEVEL%"
echo.
pause
exit /b %EXITCODE%

# POWERSHELL_PAYLOAD
$GatewayOrigin = "https://api.snowsome.com"
$LoginEndpoint = "$GatewayOrigin/admin/api/login"
$ProvisionEndpoint = "$GatewayOrigin/api/provision/client-config"
$DefaultUserPassword = "123456"
$ApprovalPollSeconds = 5
$ApprovalTimeoutSeconds = 60

function New-JsonObject {
    return New-Object PSObject
}

function Set-JsonProperty($Object, [string] $Name, $Value) {
    $property = $Object.PSObject.Properties[$Name]
    if ($property) {
        $property.Value = $Value
        return
    }
    $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value
}

function Read-JsonObject([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return New-JsonObject
    }

    $raw = Get-Content -LiteralPath $Path -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) {
        return New-JsonObject
    }

    $value = $raw | ConvertFrom-Json
    if ($value -is [System.Array]) {
        throw "Expected a JSON object in $Path"
    }
    return $value
}

function Backup-IfExists([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return $null
    }

    $backupPath = "$Path.bak-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
    Copy-Item -LiteralPath $Path -Destination $backupPath -Force
    return $backupPath
}

function Write-JsonFile([string] $Path, $Value) {
    $directory = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $directory)) {
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
    }

    $json = $Value | ConvertTo-Json -Depth 20
    $encoding = New-Object System.Text.UTF8Encoding -ArgumentList $false
    [System.IO.File]::WriteAllText($Path, $json + [Environment]::NewLine, $encoding)
}

function Write-TextFile([string] $Path, [string] $Value) {
    $directory = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $directory)) {
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
    }

    $encoding = New-Object System.Text.UTF8Encoding -ArgumentList $false
    [System.IO.File]::WriteAllText($Path, $Value, $encoding)
}

function Write-Color([string] $Text = "", [ConsoleColor] $Color = [ConsoleColor]::Gray) {
    if ([string]::IsNullOrEmpty($Text)) {
        Write-Host ""
        return
    }
    Write-Host $Text -ForegroundColor $Color
}

function Write-Title([string] $Text) {
    Write-Color "============================================================" Cyan
    Write-Color "  $Text" Cyan
    Write-Color "============================================================" Cyan
}

function Write-Step([string] $Text) {
    Write-Color "[*] $Text" Yellow
}

function Write-Ok([string] $Text) {
    Write-Color "[OK] $Text" Green
}

function Write-Note([string] $Text) {
    Write-Color "    $Text" DarkGray
}

function Write-BackupStatus([string] $Name, [string] $BackupPath) {
    if ($BackupPath) {
        Write-Color "[BACKUP] $Name config existed, backup created:" Magenta
        Write-Color "         $BackupPath" Magenta
        return
    }
    Write-Note "$Name config did not exist; a new file will be created."
}

function ConvertTo-PlainText([Security.SecureString] $Value) {
    $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($Value)
    try {
        return [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
    } finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)
    }
}

function Test-WebLogin([string] $Username, [string] $Password, $Session) {
    $body = @{ username = $Username; password = $Password } | ConvertTo-Json
    try {
        $login = Invoke-RestMethod -Method Post -Uri $LoginEndpoint -ContentType "application/json" -Body $body -WebSession $Session
    } catch {
        return $false
    }
    return ($login.ok -eq $true -and -not [string]::IsNullOrWhiteSpace([string] $login.role))
}

function Test-GatewayReady {
    $url = "$GatewayOrigin/admin/login"
    Write-Step "Checking gateway: $url"
    try {
        $response = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 8
    } catch {
        throw "Gateway is not reachable at $url. Check that the gateway server is running and the port is open. $($_.Exception.Message)"
    }
    if ($response.StatusCode -lt 200 -or $response.StatusCode -ge 500) {
        throw "Gateway returned unexpected HTTP status $($response.StatusCode) at $url."
    }
    Write-Ok "Gateway is reachable."
}

function Get-ClientModelName([string] $ModelName) {
    $modelText = $ModelName.Trim()
    if ($modelText.ToLowerInvariant().StartsWith("midas-")) {
        return $modelText
    }
    return "midas-$modelText"
}

function Get-CodeBuddyModelId([string] $ModelName) {
    return Get-ClientModelName $ModelName
}

function Get-ModelLimitValue($ModelLimits, [string] $ModelName, [string] $PropertyName, [int] $DefaultValue) {
    if ($null -eq $ModelLimits) {
        return $DefaultValue
    }

    $entry = $null
    if ($ModelLimits -is [System.Collections.IDictionary]) {
        if ($ModelLimits.Contains($ModelName)) {
            $entry = $ModelLimits[$ModelName]
        }
    } else {
        $property = $ModelLimits.PSObject.Properties[$ModelName]
        if ($property) {
            $entry = $property.Value
        }
    }

    if ($null -eq $entry) {
        return $DefaultValue
    }

    $valueProperty = $entry.PSObject.Properties[$PropertyName]
    if (-not $valueProperty) {
        return $DefaultValue
    }

    $parsed = 0
    if ([int]::TryParse([string] $valueProperty.Value, [ref] $parsed) -and $parsed -gt 0) {
        return $parsed
    }
    return $DefaultValue
}

function Get-ModelDetail($ModelDetails, [string] $ModelName) {
    if ($null -eq $ModelDetails) {
        return $null
    }

    foreach ($entry in @($ModelDetails)) {
        if ($null -eq $entry -or $entry -is [string]) {
            continue
        }

        $idProperty = $entry.PSObject.Properties["id"]
        if ($idProperty -and [string] $idProperty.Value -eq $ModelName) {
            return $entry
        }

        $providerModelProperty = $entry.PSObject.Properties["provider_model"]
        if ($providerModelProperty -and [string] $providerModelProperty.Value -eq $ModelName) {
            return $entry
        }
    }
    return $null
}

function Get-ModelDetailInt($ModelDetails, [string] $ModelName, [string[]] $PropertyNames, [int] $DefaultValue) {
    $detail = Get-ModelDetail $ModelDetails $ModelName
    if ($null -eq $detail) {
        return $DefaultValue
    }

    foreach ($propertyName in $PropertyNames) {
        $property = $detail.PSObject.Properties[$propertyName]
        if (-not $property -or $null -eq $property.Value) {
            continue
        }

        $parsed = 0
        if ([int]::TryParse([string] $property.Value, [ref] $parsed) -and $parsed -gt 0) {
            return $parsed
        }
    }
    return $DefaultValue
}

function Get-ModelDetailBool($ModelDetails, [string] $ModelName, [string] $PropertyName, [bool] $DefaultValue) {
    $detail = Get-ModelDetail $ModelDetails $ModelName
    if ($null -eq $detail) {
        return $DefaultValue
    }

    $property = $detail.PSObject.Properties[$PropertyName]
    if (-not $property -or $null -eq $property.Value) {
        return $DefaultValue
    }

    if ($property.Value -is [bool]) {
        return $property.Value
    }

    $text = ([string] $property.Value).Trim().ToLowerInvariant()
    if ($text -in @("true", "1", "yes")) {
        return $true
    }
    if ($text -in @("false", "0", "no")) {
        return $false
    }
    return $DefaultValue
}

function Get-ModelDetailPricing($ModelDetails, [string] $ModelName, [string] $PropertyName, [double] $DefaultValue) {
    $detail = Get-ModelDetail $ModelDetails $ModelName
    if ($null -eq $detail) {
        return $DefaultValue
    }

    $pricingProperty = $detail.PSObject.Properties["pricing"]
    if (-not $pricingProperty -or $null -eq $pricingProperty.Value) {
        return $DefaultValue
    }

    $valueProperty = $pricingProperty.Value.PSObject.Properties[$PropertyName]
    if (-not $valueProperty -or $null -eq $valueProperty.Value) {
        return $DefaultValue
    }

    $parsed = 0.0
    if ([double]::TryParse([string] $valueProperty.Value, [ref] $parsed) -and $parsed -ge 0) {
        return $parsed
    }
    return $DefaultValue
}

function New-XcodeModelInfo($ModelDetails, $ModelLimits, [string] $ModelName) {
    $contextDefault = Get-ModelLimitValue $ModelLimits $ModelName "max_input_tokens" 200000
    $contextWindow = Get-ModelDetailInt $ModelDetails $ModelName @("max_input_tokens", "context_window") $contextDefault
    $supportsImages = Get-ModelDetailBool $ModelDetails $ModelName "supports_image" $false
    $supportsTools = Get-ModelDetailBool $ModelDetails $ModelName "supports_tools" $true
    $inputPrice = Get-ModelDetailPricing $ModelDetails $ModelName "input_per_1m" 0.0
    $outputPrice = Get-ModelDetailPricing $ModelDetails $ModelName "output_per_1m" 0.0
    $cacheReadPrice = Get-ModelDetailPricing $ModelDetails $ModelName "cached_input_per_1m" 0.0

    $inputModalities = @("text")
    if ($supportsImages) {
        $inputModalities += "image"
    }

    $tier = New-JsonObject
    Set-JsonProperty $tier "name" "standard"
    Set-JsonProperty $tier "max_context_tokens" $contextWindow
    Set-JsonProperty $tier "input" $inputPrice
    Set-JsonProperty $tier "output" $outputPrice
    Set-JsonProperty $tier "cache_read" $cacheReadPrice

    $pricing = New-JsonObject
    Set-JsonProperty $pricing "currency" "CNY"
    Set-JsonProperty $pricing "unit" "per_1m_tokens"
    Set-JsonProperty $pricing "tiers" @($tier)

    $reasoningPreset = New-JsonObject
    Set-JsonProperty $reasoningPreset "effort" "medium"
    Set-JsonProperty $reasoningPreset "description" "Default reasoning"

    $truncationPolicy = New-JsonObject
    Set-JsonProperty $truncationPolicy "mode" "tokens"
    Set-JsonProperty $truncationPolicy "limit" $contextWindow

    $serviceTier = New-JsonObject
    Set-JsonProperty $serviceTier "id" "priority"
    Set-JsonProperty $serviceTier "name" "Priority"
    Set-JsonProperty $serviceTier "description" "MIDAS priority service tier"

    $model = New-JsonObject
    Set-JsonProperty $model "slug" $ModelName
    Set-JsonProperty $model "display_name" $ModelName
    Set-JsonProperty $model "description" "MIDAS Gateway model"
    Set-JsonProperty $model "default_reasoning_level" "medium"
    Set-JsonProperty $model "supported_reasoning_levels" @($reasoningPreset)
    Set-JsonProperty $model "shell_type" "shell_command"
    Set-JsonProperty $model "visibility" "list"
    Set-JsonProperty $model "supported_in_api" $true
    Set-JsonProperty $model "priority" 0
    Set-JsonProperty $model "availability_nux" $null
    Set-JsonProperty $model "upgrade" $null
    Set-JsonProperty $model "base_instructions" "You are Codex, a coding agent. Follow the active system and developer instructions."
    Set-JsonProperty $model "supports_reasoning_summaries" $false
    Set-JsonProperty $model "support_verbosity" $false
    Set-JsonProperty $model "default_verbosity" $null
    Set-JsonProperty $model "apply_patch_tool_type" "freeform"
    Set-JsonProperty $model "truncation_policy" $truncationPolicy
    Set-JsonProperty $model "supports_parallel_tool_calls" $supportsTools
    Set-JsonProperty $model "supports_image_detail_original" $false
    Set-JsonProperty $model "context_window" $contextWindow
    Set-JsonProperty $model "max_context_window" $contextWindow
    Set-JsonProperty $model "experimental_supported_tools" @()
    Set-JsonProperty $model "input_modalities" $inputModalities
    Set-JsonProperty $model "pricing" $pricing
    Set-JsonProperty $model "default_service_tier" "priority"
    Set-JsonProperty $model "service_tiers" @($serviceTier)
    return $model
}

function Set-TomlTopLevelString([string] $Text, [string] $Name, [string] $Value, [bool] $LiteralString) {
    if ($LiteralString) {
        $line = "$Name = '$Value'"
    } else {
        $escapedValue = $Value.Replace("\", "\\").Replace('"', '\"')
        $line = "$Name = `"$escapedValue`""
    }

    $pattern = "(?m)^$([regex]::Escape($Name))\s*=.*$"
    if ([regex]::IsMatch($Text, $pattern)) {
        $safeLine = $line -replace "\$", '$$'
        return [regex]::Replace($Text, $pattern, $safeLine)
    }
    if ([string]::IsNullOrWhiteSpace($Text)) {
        return $line + [Environment]::NewLine
    }
    return $line + [Environment]::NewLine + $Text
}

function Remove-TomlTable([string] $Text, [string] $TableName) {
    if ([string]::IsNullOrWhiteSpace($Text)) {
        return ""
    }

    $pattern = "(?ms)^\[$([regex]::Escape($TableName))\]\r?\n.*?(?=^\[|\z)"
    return [regex]::Replace($Text, $pattern, "")
}

function Update-XcodeConfigToml([string] $Text, [string] $ModelName, [string] $ProviderId, [string] $CatalogPath, [string] $GatewayBase, [string] $ApiKey) {
    $updated = if ($null -eq $Text) { "" } else { $Text }
    $oldProviderId = ""
    $providerMatch = [regex]::Match($updated, '(?m)^model_provider\s*=\s*"([^"]+)"')
    if ($providerMatch.Success) {
        $oldProviderId = $providerMatch.Groups[1].Value
    }

    if (-not [string]::IsNullOrWhiteSpace($oldProviderId) -and $oldProviderId.StartsWith("midas")) {
        $updated = Remove-TomlTable $updated "model_providers.$oldProviderId"
    }
    $updated = Remove-TomlTable $updated "model_providers.$ProviderId"
    $updated = Set-TomlTopLevelString $updated "model" $ModelName $false
    $updated = Set-TomlTopLevelString $updated "model_catalog_json" $CatalogPath $true
    $updated = Set-TomlTopLevelString $updated "model_provider" $ProviderId $false
    $providerBlock = @"
[model_providers.$ProviderId]
name = "snow"
base_url = "$GatewayBase/v1"
experimental_bearer_token = "$ApiKey"
wire_api = "responses"
requires_openai_auth = false
"@
    return $updated.TrimEnd() + [Environment]::NewLine + [Environment]::NewLine + $providerBlock + [Environment]::NewLine
}

Write-Title "MIDAS AI 网关客户端配置"
Write-Color "  本脚本会将 CodeBuddy、xcode 和 CodeForge 配置为使用公司分配的 AI 网关和接口密钥。" White
Write-Color "  脚本不会在窗口中显示接口密钥。" DarkGray
Write-Color ""
Write-Color "  网关地址：$GatewayOrigin" Cyan
Write-Color ""
Write-Color "  如果本机已有个人配置，脚本会先自动生成备份文件，再写入公司配置。" Yellow
Write-Color "  备份文件名格式：原文件名.bak-yyyyMMdd-HHmmss" Magenta
Write-Color "  以后如果不想使用公司配置，可以删除相关配置，或恢复脚本生成的备份文件。" Green
Write-Color ""

Test-GatewayReady

$Username = Read-Host "Enter username"
if ($null -eq $Username) {
    $Username = ""
}
$Username = $Username.Trim()
if ([string]::IsNullOrWhiteSpace($Username)) {
    throw "Username is required."
}

$PasswordSecure = Read-Host "Enter web password (press Enter for default 123456)" -AsSecureString
$Password = ConvertTo-PlainText $PasswordSecure
if ([string]::IsNullOrWhiteSpace($Password)) {
    $Password = $DefaultUserPassword
}
$WebSession = New-Object Microsoft.PowerShell.Commands.WebRequestSession
if (Test-WebLogin $Username $Password $WebSession) {
    Write-Ok "Web login verified."
} else {
    throw "Web login failed. Check username and password, then try again."
}

Write-Step "Submitting setup request for user: $Username"
$requestBody = @{ username = $Username } | ConvertTo-Json
try {
    $request = Invoke-RestMethod -Method Post -Uri $ProvisionEndpoint -ContentType "application/json" -Body $requestBody -WebSession $WebSession
} catch {
    throw "Provision request failed. Check gateway address, port, server status, and web password for approved users. $($_.Exception.Message)"
}

$response = $null
$initialStatus = [string] $request.status
if ($initialStatus -eq "approved" -and -not [string]::IsNullOrWhiteSpace([string] $request.api_key)) {
    $response = $request
    Write-Ok "Request approved automatically."
} else {
    $requestToken = [string] $request.request_token
    if ([string]::IsNullOrWhiteSpace($requestToken)) {
        throw "Gateway response did not include a provision request token."
    }

    Write-Ok "Request submitted. Waiting for administrator approval."
    Write-Note "Polling every $ApprovalPollSeconds seconds. Timeout: $ApprovalTimeoutSeconds seconds."

    $deadline = (Get-Date).AddSeconds($ApprovalTimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds $ApprovalPollSeconds
        try {
            $statusResponse = Invoke-RestMethod -Method Get -Uri "$ProvisionEndpoint/$requestToken"
        } catch {
            throw "Provision approval check failed. $($_.Exception.Message)"
        }

        $status = [string] $statusResponse.status
        if ($status -eq "approved") {
            $response = $statusResponse
            break
        }
        if ($status -eq "pending") {
            Write-Note "Still waiting for approval..."
            continue
        }
        if ($status -eq "completed") {
            throw "This provision request was already completed. Run this setup file again to create a new request."
        }
        throw "Provision request ended with status '$status'."
    }

    if ($null -eq $response) {
        throw "Approval was not received within $ApprovalTimeoutSeconds seconds. Setup stopped."
    }
}

$ApiKey = [string] $response.api_key
$Models = @()
if ($response.PSObject.Properties["models"] -and $null -ne $response.models) {
    foreach ($modelName in @($response.models)) {
        $modelText = [string] $modelName
        if (-not [string]::IsNullOrWhiteSpace($modelText)) {
            $Models += $modelText.Trim()
        }
    }
}
if ($Models.Count -eq 0) {
    $modelText = [string] $response.model
    if (-not [string]::IsNullOrWhiteSpace($modelText)) {
        $Models += $modelText.Trim()
    }
}
if ([string]::IsNullOrWhiteSpace($ApiKey) -or $Models.Count -eq 0) {
    throw "Gateway response did not include api_key and model."
}
$ModelLimits = $null
if ($response.PSObject.Properties["model_limits"] -and $null -ne $response.model_limits) {
    $ModelLimits = $response.model_limits
}
$ModelDetails = @()
if ($response.PSObject.Properties["model_details"] -and $null -ne $response.model_details) {
    foreach ($modelDetail in @($response.model_details)) {
        if ($null -ne $modelDetail -and -not ($modelDetail -is [string])) {
            $ModelDetails += $modelDetail
        }
    }
}
$Model = $Models[0]
$ClientModels = @()
foreach ($modelName in $Models) {
    $ClientModels += (Get-ClientModelName $modelName)
}
$ClientModel = $ClientModels[0]

$GatewayBase = $GatewayOrigin.TrimEnd("/")
$CodeBuddyPath = Join-Path $env:USERPROFILE ".codebuddy\models.json"
$XcodeHome = Join-Path $env:USERPROFILE ".xcode"
$XcodeConfigPath = Join-Path $XcodeHome "config.toml"
$XcodeCatalogPath = Join-Path $XcodeHome "midas-models.json"
$CodeForgeHome = Join-Path $env:USERPROFILE ".codeforge"
$CodeForgeConfigPath = Join-Path $CodeForgeHome "config.toml"
$CodeForgeCatalogPath = Join-Path $CodeForgeHome "midas-models.json"
$XcodeProviderId = "midas-gateway"
$backups = @()

Write-Color ""
Write-Step "Config files to update:"
Write-Note $CodeBuddyPath
Write-Note $XcodeConfigPath
Write-Note $XcodeCatalogPath
Write-Note $CodeForgeConfigPath
Write-Note $CodeForgeCatalogPath
Write-Color ""

Write-Color ""
Write-Step "Updating CodeBuddy config"
$codeBuddySettings = Read-JsonObject $CodeBuddyPath
$existingCodeBuddyModels = @()
if ($codeBuddySettings.PSObject.Properties["models"] -and $null -ne $codeBuddySettings.models -and -not ($codeBuddySettings.models -is [string])) {
    foreach ($existingModel in @($codeBuddySettings.models)) {
        if ($null -ne $existingModel -and -not ($existingModel -is [string])) {
            $existingCodeBuddyModels += $existingModel
        }
    }
}

$GatewayChatUrl = "$GatewayBase/v1/chat/completions"
$CodeBuddyModelName = "snow"
$LegacyCodeBuddyModelNames = @("snow")
$codeBuddyModelIds = @{}
$legacyCodeBuddyModelIds = @{}
foreach ($modelName in $Models) {
    $codeBuddyModelIds[(Get-CodeBuddyModelId $modelName)] = $true
    $legacyCodeBuddyModelIds[$modelName] = $true
}

$oldGatewayModelIds = @{}
$preservedCodeBuddyModels = @()
foreach ($existingModel in $existingCodeBuddyModels) {
    $existingId = [string] $existingModel.id
    $existingName = [string] $existingModel.name
    $existingUrl = [string] $existingModel.url
    $isTargetGatewayName = $existingName -eq $CodeBuddyModelName -or $LegacyCodeBuddyModelNames -contains $existingName
    $isLegacyMidasModel = $LegacyCodeBuddyModelNames -contains $existingName -and $existingId.StartsWith("midas-")
    if (-not [string]::IsNullOrWhiteSpace($existingId) -and $isTargetGatewayName -and ($isLegacyMidasModel -or $codeBuddyModelIds.ContainsKey($existingId) -or $legacyCodeBuddyModelIds.ContainsKey($existingId) -or $existingUrl -eq $GatewayChatUrl)) {
        $oldGatewayModelIds[$existingId] = $true
        continue
    }
    $preservedCodeBuddyModels += $existingModel
}

$codeBuddyModels = @()
foreach ($modelName in $Models) {
    $codeBuddyModelId = Get-CodeBuddyModelId $modelName
    $maxInputTokensDefault = Get-ModelLimitValue $ModelLimits $modelName "max_input_tokens" 200000
    $maxOutputTokensDefault = Get-ModelLimitValue $ModelLimits $modelName "max_output_tokens" 8192
    $maxInputTokens = Get-ModelDetailInt $ModelDetails $modelName @("max_input_tokens", "context_window") $maxInputTokensDefault
    $maxOutputTokens = Get-ModelDetailInt $ModelDetails $modelName @("max_output_tokens") $maxOutputTokensDefault
    $supportsToolCall = Get-ModelDetailBool $ModelDetails $modelName "supports_tools" $true
    $supportsImages = Get-ModelDetailBool $ModelDetails $modelName "supports_image" $false
    $supportsReasoning = Get-ModelDetailBool $ModelDetails $modelName "supports_reasoning" $true
    $codeBuddyModel = New-JsonObject
    Set-JsonProperty $codeBuddyModel "id" $codeBuddyModelId
    Set-JsonProperty $codeBuddyModel "name" $CodeBuddyModelName
    Set-JsonProperty $codeBuddyModel "vendor" "MIDAS"
    Set-JsonProperty $codeBuddyModel "apiKey" $ApiKey
    Set-JsonProperty $codeBuddyModel "maxInputTokens" $maxInputTokens
    Set-JsonProperty $codeBuddyModel "maxOutputTokens" $maxOutputTokens
    Set-JsonProperty $codeBuddyModel "url" "$GatewayBase/v1/chat/completions"
    Set-JsonProperty $codeBuddyModel "temperature" 1
    Set-JsonProperty $codeBuddyModel "supportsToolCall" $supportsToolCall
    Set-JsonProperty $codeBuddyModel "supportsImages" $supportsImages
    Set-JsonProperty $codeBuddyModel "supportsReasoning" $supportsReasoning
    $codeBuddyModels += $codeBuddyModel
}

Set-JsonProperty $codeBuddySettings "models" ($preservedCodeBuddyModels + $codeBuddyModels)
if ($codeBuddySettings.PSObject.Properties["availableModels"]) {
    $availableSeen = @{}
    $nonGatewayAvailableModels = @()
    foreach ($availableModel in @($codeBuddySettings.availableModels)) {
        $availableId = [string] $availableModel
        if ([string]::IsNullOrWhiteSpace($availableId)) {
            continue
        }
        if ($oldGatewayModelIds.ContainsKey($availableId) -or $codeBuddyModelIds.ContainsKey($availableId) -or $legacyCodeBuddyModelIds.ContainsKey($availableId)) {
            continue
        }
        if (-not $availableSeen.ContainsKey($availableId)) {
            $availableSeen[$availableId] = $true
            $nonGatewayAvailableModels += $availableId
        }
    }
    if ($nonGatewayAvailableModels.Count -eq 0) {
        $codeBuddySettings.PSObject.Properties.Remove("availableModels")
    } else {
        foreach ($modelName in $Models) {
            $codeBuddyModelId = Get-CodeBuddyModelId $modelName
            if (-not $availableSeen.ContainsKey($codeBuddyModelId)) {
                $availableSeen[$codeBuddyModelId] = $true
                $nonGatewayAvailableModels += $codeBuddyModelId
            }
        }
        Set-JsonProperty $codeBuddySettings "availableModels" $nonGatewayAvailableModels
    }
}
$backup = Backup-IfExists $CodeBuddyPath
if ($backup) { $backups += $backup }
Write-BackupStatus "CodeBuddy" $backup
Write-JsonFile $CodeBuddyPath $codeBuddySettings
Write-Ok "CodeBuddy config updated."

Write-Color ""
Write-Step "Updating xcode config"
$xcodeModels = @()
foreach ($modelName in $Models) {
    $xcodeModels += (New-XcodeModelInfo $ModelDetails $ModelLimits $modelName)
}
$xcodeCatalog = New-JsonObject
Set-JsonProperty $xcodeCatalog "models" $xcodeModels
$backup = Backup-IfExists $XcodeCatalogPath
if ($backup) { $backups += $backup }
Write-BackupStatus "xcode model catalog" $backup
Write-JsonFile $XcodeCatalogPath $xcodeCatalog

$xcodeConfigText = ""
if (Test-Path -LiteralPath $XcodeConfigPath) {
    $xcodeConfigText = [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath $XcodeConfigPath), [System.Text.Encoding]::UTF8)
}
$xcodeConfigText = Update-XcodeConfigToml $xcodeConfigText $Model $XcodeProviderId $XcodeCatalogPath $GatewayBase $ApiKey
$backup = Backup-IfExists $XcodeConfigPath
if ($backup) { $backups += $backup }
Write-BackupStatus "xcode" $backup
Write-TextFile $XcodeConfigPath $xcodeConfigText
Write-Ok "xcode config updated."

Write-Color ""
Write-Step "Updating CodeForge config"
$backup = Backup-IfExists $CodeForgeCatalogPath
if ($backup) { $backups += $backup }
Write-BackupStatus "CodeForge model catalog" $backup
Write-JsonFile $CodeForgeCatalogPath $xcodeCatalog

$codeForgeConfigText = ""
if (Test-Path -LiteralPath $CodeForgeConfigPath) {
    $codeForgeConfigText = [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath $CodeForgeConfigPath), [System.Text.Encoding]::UTF8)
}
$codeForgeConfigText = Update-XcodeConfigToml $codeForgeConfigText $Model $XcodeProviderId $CodeForgeCatalogPath $GatewayBase $ApiKey
$backup = Backup-IfExists $CodeForgeConfigPath
if ($backup) { $backups += $backup }
Write-BackupStatus "CodeForge" $backup
Write-TextFile $CodeForgeConfigPath $codeForgeConfigText
Write-Ok "CodeForge config updated."

Write-Color ""
Write-Title "Setup Complete"
Write-Ok "Updated config files:"
Write-Color "  $CodeBuddyPath" Green
Write-Color "  $XcodeConfigPath" Green
Write-Color "  $XcodeCatalogPath" Green
Write-Color "  $CodeForgeConfigPath" Green
Write-Color "  $CodeForgeCatalogPath" Green
Write-Color ""
Write-Color "Default model: $ClientModel" Cyan
Write-Color "Available models: $($ClientModels -join ', ')" Cyan
Write-Color "API key was written to the config files and is not printed here." DarkGray
if ($backups.Count -gt 0) {
    Write-Color ""
    Write-Color "Backup file(s) created from existing config:" Magenta
    foreach ($backupPath in $backups) {
        Write-Color "  $backupPath" Magenta
    }
} else {
    Write-Color ""
    Write-Color "No old config files were found, so no backup files were needed." DarkGray
}
