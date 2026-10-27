# GPU testing and Kata passthrough plan

Status: investigation and read-only diagnostics, 2026-10-27. Physical GPU
passthrough and CUDA execution are **not implemented**. The existing guest
Vulkan/OpenCL diagnostics do not grant access to host GPUs.

## Reviewed path and limits

Jbox currently starts Docker with the `kata` runtime, a read-only root,
restricted capabilities and no GPU or VFIO device request. A physical GPU cannot
be enabled merely by installing a CUDA toolkit or by registering Docker's NVIDIA
runtime. The host driver, device assignment, Kata VM kernel and guest-side
NVIDIA driver/runtime must all work together.

The upstream [Kata NVIDIA GPU guide](https://github.com/kata-containers/kata-containers/blob/main/docs/use-cases/NVIDIA-GPU-passthrough-and-Kata.md)
describes exclusive PCI/VFIO passthrough, IOMMU grouping, and a GPU-capable
guest kernel and driver. Its examples use `ctr`, not Jbox's Docker integration.
The newer [Kata NVIDIA QEMU reference design](https://github.com/kata-containers/kata-containers/blob/main/docs/use-cases/NVIDIA-GPU-passthrough-and-Kata-QEMU.md)
describes cold-plug, host and guest CDI, and Kubernetes allocation. Do not
assume its Kubernetes-specific discovery and annotations work with Docker.
The locally installed runtime-rs 4.3.1 source contains standalone OCI-device
discovery for VFIO cold-plug, so Kubernetes is not necessarily required. This
is source-level evidence, not a successful Docker or Jbox hardware test.
The packaged NVIDIA configuration sets `shared_fs = "none"`, whereas Jbox
needs worktree and runtime bind mounts. A compatible, independently configured
GPU runtime must preserve those mounts and the existing isolation properties.
Guest-side GPU device and driver injection must also be verified. Confirm the
exact Docker, Kata runtime-rs and device-injection path experimentally before
implementing it. vGPU, MIG and shared-device use are out of initial scope.

### Local, read-only observations

On the current machine, the RTX 4050 at `0000:01:00.0` uses the host `nvidia`
driver, and its audio function `0000:01:00.1` uses `snd_hda_intel`. Neither
showed an IOMMU group and `/sys/kernel/iommu_groups` was empty. The VFIO control
node existed, but no VFIO group node was present and `vfio_pci` was not loaded.
The registered `kata` runtime references the ordinary QEMU runtime-rs
configuration. The installation also contains an NVIDIA-specific QEMU
configuration and its referenced GPU kernel and guest image, but their
existence does not establish Docker compatibility or readiness. All observations
are snapshots, not persistent guarantees.

Changing firmware or boot parameters, binding a GPU to VFIO, or changing Kata's
host configuration can disrupt graphics, compute workloads, suspend/resume or
the entire host. Jbox will not perform those actions implicitly. Passthrough
changes the guest's attack surface and must account for the entire IOMMU group,
which may include devices other than the selected GPU. Granting access to a VFIO
group can have broader effects than the selected function. Passthrough also
generally removes the assigned GPU from normal host use while assigned.

## Staged work

1. **Read-only readiness (`jbox-tng`).** Report detected PCI display devices,
   their drivers, IOMMU groups and group members where discoverable. Describe
   inaccessible or absent information as unknown, not as proven readiness.
   Make no host changes, and always report that Jbox does not yet implement GPU
   assignment. Cover sysfs error handling and multi-device groups.
2. **Design and feasibility (`jbox-yvv`).** Establish a supported Docker plus
   runtime-rs GPU attachment mechanism and guest driver injection with an
   explicit, separate GPU-capable runtime configuration. Define narrow device
   selection, group membership policy, exclusive ownership, lifecycle cleanup,
   races, resume behavior, and fail-closed error handling. Review security and
   rollback before implementation. Avoid arbitrary user-supplied device paths,
   blanket privileges and host driver mounts.
3. **Opt-in assignment (`jbox-bw6`).** Only after the design and runtime path
   are validated, implement explicit per-project or per-session selection and
   tests for defaults, malformed requests, contention and lifecycle failures.
   Existing no-GPU sessions must remain unchanged. Do not switch all guests to
   a GPU runtime or silently fall back to weaker isolation.
4. **CUDA tooling (`jbox-efk`, `jbox-ru6`).** The default image provides a
   CUDA compiler and runtime development files. Complete the guest-driver and
   user-space integration after the runtime architecture is known. Keep
   project-specific frameworks and dependencies in project environments.
   Verify version and license compatibility, updates and image size. Do not
   claim that `nvcc` or a host NVIDIA driver alone enables the guest GPU.
5. **Real hardware acceptance (`jbox-699`).** On an explicitly prepared host,
   independently verify device isolation, full group membership, GPU identity,
   guest driver and CUDA versions, a real kernel execution with checked results,
   and normal and failure cleanup. Confirm default guests receive no device and
   document host recovery. Never unbind the user's GPU or change boot/runtime
   configuration without explicit approval and a recovery plan.

The Beads epic is `jbox-7q6`; dependencies prevent implementation and acceptance
from being treated as complete before the design and prerequisites are resolved.
