# Açık bir tarayıcı bağlam menüsünde adı verilen öğeyi Windows UI Automation ile bulup çağırır
# (kullanıcının tıklamasıyla aynı). edge-menu.mjs kullanır.
param([Parameter(Mandatory)][string]$Name, [int]$TimeoutSec = 10)
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$ae = [System.Windows.Automation.AutomationElement]
$cond = New-Object System.Windows.Automation.PropertyCondition($ae::NameProperty, $Name)
$deadline = (Get-Date).AddSeconds($TimeoutSec)
while ((Get-Date) -lt $deadline) {
    $el = $ae::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
    if ($el) {
        $p = $null
        if ($el.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) {
            $p.Invoke()
            "çağrıldı: $Name"
            exit 0
        }
        "öğe bulundu ama Invoke desteklemiyor: $Name"
        exit 2
    }
    Start-Sleep -Milliseconds 300
}
"bulunamadı: $Name"
exit 1
