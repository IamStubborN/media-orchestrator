variable "SERVICE_IMAGE" {
  default = "media-orchestrator-service:local"
}

variable "RUNNER_IMAGE" {
  default = "media-orchestrator-runner:local"
}

variable "OCI_CREATED" {
  default = ""
}

variable "OCI_REVISION" {
  default = ""
}

variable "OCI_RUNNER_BUILD_DIGEST" {
  default = ""
}

variable "OCI_SOURCE" {
  default = ""
}

variable "OCI_SOURCE_TREE_DIGEST" {
  default = ""
}

variable "OCI_VERSION" {
  default = ""
}

target "service" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "service"
  tags       = [SERVICE_IMAGE]
  args = {
    OCI_CREATED             = OCI_CREATED
    OCI_REVISION            = OCI_REVISION
    OCI_RUNNER_BUILD_DIGEST = OCI_RUNNER_BUILD_DIGEST
    OCI_SOURCE              = OCI_SOURCE
    OCI_SOURCE_TREE_DIGEST  = OCI_SOURCE_TREE_DIGEST
    OCI_VERSION             = OCI_VERSION
  }
}

target "runner" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "runner"
  tags       = [RUNNER_IMAGE]
  args = {
    OCI_CREATED             = OCI_CREATED
    OCI_REVISION            = OCI_REVISION
    OCI_RUNNER_BUILD_DIGEST = OCI_RUNNER_BUILD_DIGEST
    OCI_SOURCE              = OCI_SOURCE
    OCI_SOURCE_TREE_DIGEST  = OCI_SOURCE_TREE_DIGEST
    OCI_VERSION             = OCI_VERSION
  }
}

group "default" {
  targets = ["service", "runner"]
}

group "service-only" {
  targets = ["service"]
}

group "runner-only" {
  targets = ["runner"]
}
