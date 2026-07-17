{{/* Expand the chart name. */}}
{{- define "spurfire-control.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* Create a DNS-safe fully qualified name. */}}
{{- define "spurfire-control.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* Chart label. */}}
{{- define "spurfire-control.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* Stable selector labels. */}}
{{- define "spurfire-control.selectorLabels" -}}
app.kubernetes.io/name: {{ include "spurfire-control.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/* Common metadata labels. */}}
{{- define "spurfire-control.labels" -}}
helm.sh/chart: {{ include "spurfire-control.chart" . }}
{{ include "spurfire-control.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/component: control-plane
app.kubernetes.io/part-of: spurfire
{{- end -}}

{{/* Service account name. */}}
{{- define "spurfire-control.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "spurfire-control.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/* Image reference; a digest takes precedence over a mutable tag. */}}
{{- define "spurfire-control.image" -}}
{{- if .Values.image.digest -}}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.image.repository (default .Chart.AppVersion .Values.image.tag) -}}
{{- end -}}
{{- end -}}

{{/* Claim name for the non-secret JSON state directory. */}}
{{- define "spurfire-control.stateClaimName" -}}
{{- default (include "spurfire-control.fullname" .) .Values.persistence.existingClaim -}}
{{- end -}}

{{/* Fail early on unsafe or contradictory mode combinations. */}}
{{- define "spurfire-control.validateValues" -}}
{{- if and .Values.config.dryRun (ne .Values.config.provisioningMode "dry_run") -}}
{{- fail "config.dryRun=true requires config.provisioningMode=\"dry_run\"" -}}
{{- end -}}
{{- if and (not .Values.config.dryRun) (eq .Values.config.provisioningMode "dry_run") -}}
{{- fail "config.provisioningMode=\"dry_run\" requires config.dryRun=true" -}}
{{- end -}}
{{- if and (not .Values.config.dryRun) (empty .Values.tailscale.existingSecret) -}}
{{- fail "real mode requires tailscale.existingSecret" -}}
{{- end -}}
{{- if and (not .Values.config.dryRun) (not .Values.persistence.enabled) -}}
{{- fail "real mode requires persistence.enabled=true" -}}
{{- end -}}
{{- if and .Values.httpRoute.enabled (empty .Values.httpRoute.parentRefs) -}}
{{- fail "httpRoute.enabled=true requires at least one parentRef" -}}
{{- end -}}
{{- if and .Values.httpRoute.enabled (empty .Values.httpRoute.hostnames) -}}
{{- fail "httpRoute.enabled=true requires at least one hostname" -}}
{{- end -}}
{{- range $label := list "app.kubernetes.io/name" "app.kubernetes.io/instance" "app.kubernetes.io/component" "app.kubernetes.io/part-of" -}}
{{- if hasKey $.Values.podLabels $label -}}
{{- fail (printf "podLabels must not override reserved label %s" $label) -}}
{{- end -}}
{{- end -}}
{{- if hasKey .Values.podAnnotations "checksum/config" -}}
{{- fail "podAnnotations must not override checksum/config" -}}
{{- end -}}
{{- if hasKey .Values.persistence.annotations "helm.sh/resource-policy" -}}
{{- fail "persistence.annotations must not set helm.sh/resource-policy; use persistence.retain" -}}
{{- end -}}
{{- end -}}
