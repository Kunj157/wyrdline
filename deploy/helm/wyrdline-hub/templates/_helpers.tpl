{{/* Chart name, truncated to the DNS label limit. */}}
{{- define "wyrdline-hub.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "wyrdline-hub.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "wyrdline-hub.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "wyrdline-hub.selectorLabels" -}}
app.kubernetes.io/name: {{ include "wyrdline-hub.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "wyrdline-hub.labels" -}}
helm.sh/chart: {{ include "wyrdline-hub.chart" . }}
{{ include "wyrdline-hub.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: wyrdline
{{- end }}

{{- define "wyrdline-hub.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "wyrdline-hub.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{- define "wyrdline-hub.image" -}}
{{- if .Values.image.digest }}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository (default .Chart.AppVersion .Values.image.tag) }}
{{- end }}
{{- end }}

{{/*
Fails the render with a message naming what is missing. Each check assigns to
a throwaway variable so nothing is printed.
*/}}
{{- define "wyrdline-hub.validate" -}}
{{- $_ := required "publicUrl is required: the URL users reach the Hub at, for example https://hub.example.internal" .Values.publicUrl }}
{{- $_ := required "database.existingSecret is required: a Secret with key database-url (postgres://...)" .Values.database.existingSecret }}
{{- $_ := required "s3.endpoint is required: the S3-compatible base URL" .Values.s3.endpoint }}
{{- $_ := required "s3.bucket is required" .Values.s3.bucket }}
{{- $_ := required "s3.existingSecret is required: a Secret with keys access-key-id and secret-access-key" .Values.s3.existingSecret }}
{{- if .Values.oidc.issuer }}
{{- $_ := required "oidc.clientId is required when oidc.issuer is set" .Values.oidc.clientId }}
{{- $_ := required "oidc.existingSecret is required when oidc.issuer is set: a Secret with key client-secret" .Values.oidc.existingSecret }}
{{- end }}
{{- if .Values.ingress.enabled }}
{{- $_ := required "ingress.host is required when ingress.enabled is true" .Values.ingress.host }}
{{- end }}
{{- /* A chart built from the repository has appVersion 0.0.0, which no
       release publishes: refuse it rather than pull a tag that does not exist. */}}
{{- if and (not .Values.image.digest) (not .Values.image.tag) (eq .Chart.AppVersion "0.0.0") }}
{{- fail "image.tag or image.digest is required: this chart has no release image (appVersion 0.0.0). Set image.tag to a published Hub version, or install a released chart." }}
{{- end }}
{{- end }}
