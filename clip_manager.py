import os
import sys
# CorridorKey 2.0 4K windows OOM from allocator fragmentation without this;
# must be set before torch initialises CUDA. Linux only (unsupported on Windows).
if sys.platform.startswith("linux") and "PYTORCH_CUDA_ALLOC_CONF" not in os.environ:
    os.environ.setdefault("PYTORCH_ALLOC_CONF", "expandable_segments:True")
import argparse
import logging
import glob

# Enable OpenEXR Support (Must be before cv2 import)
os.environ["OPENCV_IO_ENABLE_OPENEXR"] = "1"
import cv2
import shutil
import warnings
import numpy as np

# Suppress warnings
warnings.filterwarnings("ignore")

# Configure logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

# Core Paths
BASE_DIR = os.path.dirname(os.path.abspath(__file__))
CLIPS_DIR = os.path.join(BASE_DIR, "ClipsForInference")
OUTPUT_DIR = os.path.join(BASE_DIR, "Output")

# Network Mapping
# Windows Drive -> Linux Mount Point
WIN_DRIVE_ROOT = "V:\\"
LINUX_MOUNT_ROOT = "/mnt/ssd-storage"

# --- Helpers ---
def is_image_file(filename):
    return filename.lower().endswith(('.png', '.jpg', '.jpeg', '.exr', '.tif', '.tiff', '.bmp'))

def is_video_file(filename):
    return filename.lower().endswith(('.mp4', '.mov', '.avi', '.mkv'))

def map_path(win_path):
    r"""
    Converts a Windows path (example: V:\Projects\Shot1) to the local Linux path.
    """
    # Normalize slashes
    win_path = win_path.strip()
    
    # Check if it starts with the drive letter
    if win_path.upper().startswith(WIN_DRIVE_ROOT.upper()):
        # Remove drive letter
        rel_path = win_path[len(WIN_DRIVE_ROOT):]
        # Flip slashes
        rel_path = rel_path.replace('\\', '/')
        # Combine
        linux_path = os.path.join(LINUX_MOUNT_ROOT, rel_path)
        return linux_path
    
    # If not on V:, maybe it's already a linux path or invalid?
    return win_path

# --- Classes ---
class ClipAsset:
    def __init__(self, path, asset_type):
        self.path = path
        self.type = asset_type # 'sequence' or 'video'
        self.frame_count = 0
        self._calculate_length()

    def _calculate_length(self):
        if self.type == 'sequence':
            files = sorted([f for f in os.listdir(self.path) if is_image_file(f)])
            self.frame_count = len(files)
        elif self.type == 'video':
            cap = cv2.VideoCapture(self.path)
            if not cap.isOpened():
                self.frame_count = 0 
            else:
                self.frame_count = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))
            cap.release()

class ClipEntry:
    def __init__(self, name, root_path):
        self.name = name
        self.root_path = root_path
        self.input_asset = None
        self.alpha_asset = None
        
    def find_assets(self):
        # 1. Look for Input
        input_dir = os.path.join(self.root_path, "Input")
        
        # Check for directory first
        if os.path.isdir(input_dir):
            if not os.listdir(input_dir):
                raise ValueError(f"Clip '{self.name}': 'Input' directory is empty.")
            self.input_asset = ClipAsset(input_dir, 'sequence')
        else:
            # Check for video file (Case-Insensitive)
            candidates = glob.glob(os.path.join(self.root_path, "[Ii]nput.*"))
            candidates = [c for c in candidates if is_video_file(c)]
            
            if candidates:
                self.input_asset = ClipAsset(candidates[0], 'video')
            else:
                # Fallback: Look for ANY video file in the directory
                all_files = glob.glob(os.path.join(self.root_path, "*"))
                video_files = [f for f in all_files if is_video_file(f)]
                
                if video_files:
                    logger.info(f"Clip '{self.name}': Using '{os.path.basename(video_files[0])}' as Input.")
                    self.input_asset = ClipAsset(video_files[0], 'video')
                else:
                    raise ValueError(f"Clip '{self.name}': No 'Input' directory or video file found.")
        
        if self.input_asset.frame_count == 0:
             raise ValueError(f"Clip '{self.name}': Input asset has 0 frames or could not be read.")

        # 2. Look for Alpha
        # Check for 'AlphaHint' or 'alphahint' directory
        alpha_dir_upper = os.path.join(self.root_path, "AlphaHint")
        alpha_dir_lower = os.path.join(self.root_path, "alphahint")
        
        target_alpha_dir = None
        if os.path.isdir(alpha_dir_upper): target_alpha_dir = alpha_dir_upper
        elif os.path.isdir(alpha_dir_lower): target_alpha_dir = alpha_dir_lower
        
        if target_alpha_dir:
            if not os.listdir(target_alpha_dir):
                logging.warning(f"Clip '{self.name}': AlphaHint directory exists but is empty. Marking for generation.")
                self.alpha_asset = None
            else:
                self.alpha_asset = ClipAsset(target_alpha_dir, 'sequence')
        else:
            # Check for video file (Case-Insensitive)
            # Match AlphaHint.* or alphahint.*
            candidates = glob.glob(os.path.join(self.root_path, "[Aa]lpha[Hh]int.*"))
            candidates = [c for c in candidates if is_video_file(c)]
            
            if candidates:
                self.alpha_asset = ClipAsset(candidates[0], 'video')
            else:
                self.alpha_asset = None # Missing, needs generation

    def validate_pair(self):
        if self.input_asset and self.alpha_asset:
            if self.input_asset.frame_count != self.alpha_asset.frame_count:
                raise ValueError(f"Clip '{self.name}': Frame count mismatch! Input: {self.input_asset.frame_count}, Alpha: {self.alpha_asset.frame_count}")

# --- Logic ---

def get_gvm_processor(device='cuda'):
    try:
        from gvm_core import GVMProcessor
        return GVMProcessor(device=device)
    except ImportError:
        raise ImportError("Could not import gvm_core. Please ensure 'gvm_core' is in the project root and requirements are installed.")
    except Exception as e:
        raise RuntimeError(f"Failed to initialize GVM Processor: {e}")

def generate_alphas(clips):
    clips_to_process = [c for c in clips if c.alpha_asset is None]
    
    if not clips_to_process:
        logger.info("All clips have valid Alpha assets. No generation needed.")
        return

    logger.info(f"Found {len(clips_to_process)} clips missing Alpha.")
    
    import torch
    device = 'cuda' if torch.cuda.is_available() else 'cpu'
    
    try:
        processor = get_gvm_processor(device=device)
    except ImportError as e:
        logger.error(f"GVM Import Error: {e}")
        logger.error("Skipping GVM generation. Please install GVM requirements if you wish to use this feature.")
        return
    except Exception as e:
        logger.error(f"GVM Initialization Error: {e}")
        return
    
    for clip in clips_to_process:
        logger.info(f"Generating Alpha for: {clip.name}")
        
        alpha_output_dir = os.path.join(clip.root_path, "AlphaHint")
        if os.path.exists(alpha_output_dir):
            shutil.rmtree(alpha_output_dir)
        os.makedirs(alpha_output_dir, exist_ok=True)
        
        try:
            processor.process_sequence(
                input_path=clip.input_asset.path,
                output_dir=None,
                num_frames_per_batch=1, 
                decode_chunk_size=1,
                denoise_steps=1,
                mode='matte',
                write_video=False,
                direct_output_dir=alpha_output_dir
            )
            
            # Post-Process: Naming Convention
            generated_files = sorted([f for f in os.listdir(alpha_output_dir) if f.endswith('.png')])
            
            if not generated_files:
                 logger.error(f"GVM finished but no PNGs found in {alpha_output_dir}")
                 continue
            
            if clip.input_asset.type == 'sequence':
                in_files = sorted([f for f in os.listdir(clip.input_asset.path) if is_image_file(f)])
                stems = [os.path.splitext(f)[0] for f in in_files]
            else:
                base_name = os.path.splitext(os.path.basename(clip.input_asset.path))[0]
                stems = [base_name] * len(generated_files)

            for i, gvm_file in enumerate(generated_files):
                if i >= len(stems): break 
                
                stem = stems[i]
                new_name = f"{stem}_alphaHint_{i:04d}.png"
                
                old_path = os.path.join(alpha_output_dir, gvm_file)
                new_path = os.path.join(alpha_output_dir, new_name)
                
                if old_path != new_path:
                    os.rename(old_path, new_path)

            logger.info(f"Saved {len(generated_files)} alpha frames to {alpha_output_dir}")

        except Exception as e:
            logger.error(f"Error generating alpha for {clip.name}: {e}")
            import traceback
            traceback.print_exc()

def run_videomama(clips, chunk_size=50):
    """
    Runs VideoMaMa on clips that have VideoMamaMaskHint but NO AlphaHint.
    """
    # Process if:
    # 1. Has VideoMamaMaskHint (File or Folder, Case-Insensitive)
    # 2. AND (Alpha is Missing OR Alpha is a Video File we want to upgrade)
    
    clips_to_process = []
    clip_mask_paths = {} # Store the resolved mask path for each clip
    
    for c in clips:
        # Search for 'videomamamaskhint' asset (Strict: videomamamaskhint.ext or VideoMamaMaskHint/)
        candidates = []
        for f in os.listdir(c.root_path):
             stem, _ = os.path.splitext(f)
             if stem.lower() == "videomamamaskhint":
                 candidates.append(f)
        
        mask_asset_path = None
        has_mask = False
        
        # Priority: Directory > Video File
        # Check directories first
        for cand in candidates:
             path = os.path.join(c.root_path, cand)
             if os.path.isdir(path) and len(os.listdir(path)) > 0:
                 has_mask = True
                 mask_asset_path = path
                 break 
        
        # If no directory, check files
        if not has_mask:
            for cand in candidates:
                path = os.path.join(c.root_path, cand)
                if os.path.isfile(path) and is_video_file(path):
                    has_mask = True
                    mask_asset_path = path
                    break
        
        if not has_mask: continue
        
        # Store for later
        clip_mask_paths[c.name] = mask_asset_path

        if c.alpha_asset is None:
            clips_to_process.append(c)
        elif c.alpha_asset.type == 'video':
            clips_to_process.append(c)
            
    if not clips_to_process:
        logger.info(f"No candidates for VideoMaMa (looking for VideoMamaMaskHint + [NoAlpha OR VideoAlpha]).")
        return

    logger.info(f"Found {len(clips_to_process)} clips for VideoMaMa processing.")
    
    # Import locally...
    try:
        sys.path.append(os.path.join(BASE_DIR, "VideoMaMaInferenceModule"))
        from VideoMaMaInferenceModule.inference import load_videomama_model, run_inference
        from VideoMaMaInferenceModule.pipeline import VideoInferencePipeline
    except ImportError as e:
        logger.error(f"Failed to import VideoMaMa: {e}")
        return

    import torch
    device = 'cuda' if torch.cuda.is_available() else 'cpu'
    
    logger.info("Loading VideoMaMa Pipeline...")
    pipeline = load_videomama_model(device=device)
    
    for clip in clips_to_process:
        logger.info(f"Running VideoMaMa on: {clip.name}")
        
        # Retrieve resolved path
        mask_hint_path = clip_mask_paths[clip.name]
        logger.info(f"  Using VideoMamaMaskHint: {os.path.basename(mask_hint_path)}")
        
        alpha_output_dir = os.path.join(clip.root_path, "AlphaHint")
        
        # Load Inputs
        # 1. Input Frames (RGB)
        input_frames = []
        if clip.input_asset.type == 'video':
            cap = cv2.VideoCapture(clip.input_asset.path)
            while True:
                ret, frame = cap.read()
                if not ret: break
                input_frames.append(cv2.cvtColor(frame, cv2.COLOR_BGR2RGB))
            cap.release()
        else:
            files = sorted([f for f in os.listdir(clip.input_asset.path) if is_image_file(f)])
            for f in files:
                fpath = os.path.join(clip.input_asset.path, f)
                # Handle EXR (Float 0-1) vs Standard (Int 0-255)
                if f.lower().endswith('.exr'):
                    img = cv2.imread(fpath, cv2.IMREAD_UNCHANGED)
                    if img is not None:
                        # Normalize Float 0-1
                        img = np.clip(img, 0.0, 1.0)
                        # Linear -> sRGB (Gamma 2.2 Approximation) for VideoMaMa
                        img = img ** (1.0/2.2)
                        # 0-255 uint8
                        img = (img * 255.0).astype(np.uint8)
                        # Ensure 3 channels
                        if img.ndim == 2:
                            img = cv2.cvtColor(img, cv2.COLOR_GRAY2BGR)
                        elif img.shape[2] == 4:
                            img = cv2.cvtColor(img, cv2.COLOR_BGRA2BGR)
                else:
                    img = cv2.imread(fpath)

                if img is not None:
                    if len(input_frames) == 0:
                        logger.info(f"Debug Input Frame 0 stats: Min={img.min()}, Max={img.max()}, Mean={img.mean()}")
                    input_frames.append(cv2.cvtColor(img, cv2.COLOR_BGR2RGB))
                    
        # 2. Mask Frames
        mask_frames = []
        
        # Check if VideoMamaMaskHint is a directory or a file (video)
        if os.path.isdir(mask_hint_path):
            # Directory of Images
            mask_files = sorted([f for f in os.listdir(mask_hint_path) if is_image_file(f)])
            for f in mask_files:
                fpath = os.path.join(mask_hint_path, f)
                m = None
                
                # Handle EXR Masks
                if f.lower().endswith('.exr'):
                    m = cv2.imread(fpath, cv2.IMREAD_UNCHANGED)
                    if m is not None:
                        if m.ndim == 3: m = m[:, :, 0]
                        m = np.clip(m, 0.0, 1.0)
                        m = (m * 255.0).astype(np.uint8)
                else:
                    # Standard Masks
                    m = cv2.imread(fpath, cv2.IMREAD_GRAYSCALE)

                if m is not None:
                    # Force Binary Thresholding
                    _, m = cv2.threshold(m, 10, 255, cv2.THRESH_BINARY)
                    mask_frames.append(m)
                    
        elif os.path.isfile(mask_hint_path):
             # Handle Video File
             cap = cv2.VideoCapture(mask_hint_path)
             while True:
                 ret, frame = cap.read()
                 if not ret: break
                 # Convert to Grayscale
                 m = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
                 # Force Binary Thresholding
                 _, m = cv2.threshold(m, 10, 255, cv2.THRESH_BINARY)
                 mask_frames.append(m)
             cap.release()
             
        if mask_frames:
             m0 = mask_frames[0]
             logger.info(f"Debug Mask Frame 0 stats (Thresholded): Min={m0.min()}, Max={m0.max()}, Mean={m0.mean()}")
        
        # Validate Lengths
        num_frames = min(len(input_frames), len(mask_frames))
        input_frames = input_frames[:num_frames]
        mask_frames = mask_frames[:num_frames]

        if num_frames == 0:
            logger.error(f"Skipping {clip.name}: No valid frame pairs found.")
            continue
            
        # Run Inference
        try:
            # Prepare Output Directory First
            # Logic: If it exists as a FILE (legacy/error), delete it.
            if os.path.exists(alpha_output_dir) and not os.path.isdir(alpha_output_dir):
                logger.warning(f"Removing file '{alpha_output_dir}' to create directory.")
                os.remove(alpha_output_dir)
            
            # If there was a Video Alpha Asset (e.g. AlphaHint.mp4), rename it to backup so it doesn't conflict
            if clip.alpha_asset and clip.alpha_asset.type == 'video':
                old_path = clip.alpha_asset.path
                if os.path.exists(old_path):
                    dir_name = os.path.dirname(old_path)
                    base, ext = os.path.splitext(os.path.basename(old_path))
                    backup_path = os.path.join(dir_name, f"{base}_backup{ext}")
                    logger.info(f"Backing up existing Alpha Video: {os.path.basename(old_path)} -> {os.path.basename(backup_path)}")
                    os.rename(old_path, backup_path)
                    # Clear it from memory so we rely on the new one
                    clip.alpha_asset = None

            os.makedirs(alpha_output_dir, exist_ok=True)
            
            # Name setup
            if clip.input_asset.type == 'sequence':
                 in_names = sorted([os.path.splitext(f)[0] for f in os.listdir(clip.input_asset.path) if is_image_file(f)])
            else:
                 stem = os.path.splitext(os.path.basename(clip.input_asset.path))[0]
                 in_names = [f"{stem}_{i:05d}" for i in range(num_frames)]
            
            total_saved = 0
            
            # Iterate generator
            for chunk_frames in run_inference(pipeline, input_frames, mask_frames, chunk_size=chunk_size):
                for frame in chunk_frames:
                    if total_saved >= len(in_names): break
                    
                    if total_saved == 0:
                        logger.info(f"Debug Output Frame 0 stats: Min={frame.min()}, Max={frame.max()}, Mean={frame.mean()}, Shape={frame.shape}, Dtype={frame.dtype}")
                    
                    name = in_names[total_saved]
                    out_path = os.path.join(alpha_output_dir, f"{name}.png")
                    
                    # Convert to BGR and Save
                    cv2.imwrite(out_path, cv2.cvtColor(frame, cv2.COLOR_RGB2BGR))
                    total_saved += 1
                
                logger.info(f"  Saved {total_saved}/{num_frames} frames...")

            logger.info(f"VideoMaMa Complete: Saved {total_saved} frames to AlphaHint.")
            
            # Update clip state in memory (dummy) - re-scan will pick it up properly
            clip.alpha_asset = ClipAsset(alpha_output_dir, 'sequence')
            
        except Exception as e:
            logger.error(f"VideoMaMa failed for {clip.name}: {e}")
            import traceback
            traceback.print_exc()

def _ask_inference_params():
    """Prompt for the settings CorridorKey 2.0 supports; returns InferenceParams."""
    from backend.service import InferenceParams

    defaults = InferenceParams()
    print("\n--- Inference Settings (CorridorKey 2.0) ---")

    gamma_choice = input("Is the input sequence Linear (l) or sRGB (s)? [l/s, default s]: ").strip().lower()
    input_is_linear = gamma_choice == 'l'

    despill_strength = defaults.despill_strength
    despill_val = input(f"Enter Despill Strength (0-1) [default {defaults.despill_strength}]: ").strip()
    if despill_val:
        try:
            despill_strength = max(0.0, min(1.0, float(despill_val)))
        except ValueError:
            logger.warning(f"Invalid despill value '{despill_val}', using {defaults.despill_strength}")

    hint_choice = input("Use AlphaHint (external coarse alpha) if present? [y/N]: ").strip().lower()
    use_alpha_hint = hint_choice == 'y'

    vram_choice = input("VRAM profile (auto/low) [default auto]: ").strip().lower()
    vram_profile = 'low' if vram_choice == 'low' else 'auto'

    print("--------------------------\n")
    return InferenceParams(
        input_is_linear=input_is_linear,
        despill_strength=despill_strength,
        use_alpha_hint=use_alpha_hint,
        vram_profile=vram_profile,
    )


def run_inference(clips, params=None, interactive=True):
    """Key clips with CorridorKey 2.0 through the shared CorridorKeyService.

    One service (one resident model) is reused for all clips. A failing clip is
    reported and the next clip still runs. Returns the number of failed clips.
    """
    from backend.clip_state import ClipEntry as BackendClipEntry
    from backend.errors import CorridorKeyError
    from backend.service import CorridorKeyService, InferenceParams, OutputConfig

    runnable = [c for c in clips if c.input_asset]
    if not runnable:
        logger.info("No clips with an Input asset found.")
        return 0
    logger.info(f"Found {len(runnable)} clips for inference.")

    if params is None:
        params = _ask_inference_params() if interactive else InferenceParams()

    try:
        service = CorridorKeyService()
    except Exception as e:
        logger.error(f"Failed to initialize CorridorKey service: {e}")
        return len(runnable)

    failed = []
    for clip in runnable:
        logger.info(f"Running Inference on: {clip.name}")
        try:
            entry = BackendClipEntry(name=clip.name, root_path=clip.root_path)
            entry.find_assets()
            results = service.run_inference(
                entry, params, output_config=OutputConfig(),
                on_progress=lambda name, cur, total, **k: print(f"  {k.get('phase', 'frame')} {cur}/{total}...", end='\r'),
                on_warning=lambda msg: logger.warning(f"Clip '{clip.name}': {msg}"),
                on_status=lambda msg: logger.info(f"Clip '{clip.name}': {msg}"),
            )
        except CorridorKeyError as e:
            print("")
            logger.error(f"Clip '{clip.name}' failed: {e}")
            failed.append(clip.name)
            continue
        except Exception as e:
            print("")
            logger.error(f"Clip '{clip.name}' failed unexpectedly: {e}")
            failed.append(clip.name)
            continue
        print("")
        ok = sum(1 for r in results if r.success)
        logger.info(f"Clip {clip.name} Complete. Processed {ok}/{len(results)} frames.")

    if failed:
        logger.error(f"{len(failed)} clip(s) failed: {', '.join(failed)}")
    return len(failed)


def organize_target(target_dir):
    """
    Organizes a specific folder. 
    1. If loose video -> Rename to Input.ext (if safe).
    2. If sequence -> Move to Input/.
    3. Ensure AlphaHint and VideoMamaMaskHint folders exist.
    """
    logger.info(f"Organizing Target: {target_dir}")
    
    if not os.path.exists(target_dir):
        logger.error(f"Target directory not found: {target_dir}")
        return

    # Check for loose video
    # Strategy: Find largest video file that ISN'T named Input.*
    candidates = [f for f in os.listdir(target_dir) if is_video_file(f)]
    candidates = [f for f in candidates if not os.path.splitext(f)[0].lower() == "input"]
    
    if candidates and not os.path.exists(os.path.join(target_dir, "Input")):
        # If multiple, pick largest (heuristic for 'Main Plate')
        candidates.sort(key=lambda f: os.path.getsize(os.path.join(target_dir, f)), reverse=True)
        main_clip = candidates[0]
        ext = os.path.splitext(main_clip)[1]
        
        try:
            shutil.move(os.path.join(target_dir, main_clip), os.path.join(target_dir, f"Input{ext}"))
            logger.info(f"Renamed '{main_clip}' to 'Input{ext}'")
        except Exception as e:
            logger.error(f"Failed to rename '{main_clip}': {e}")
            
    # Check for Image Sequence (Flat)
    # Only if Input folder doesn't exist and Input video doesn't exist
    has_input_dir = os.path.isdir(os.path.join(target_dir, "Input"))
    has_input_video = any(is_video_file(f) and os.path.basename(f).lower().startswith("input") for f in os.listdir(target_dir))
    
    if not has_input_dir and not has_input_video:
        all_files = sorted(glob.glob(os.path.join(target_dir, "*")))
        image_files = [f for f in all_files if is_image_file(f)]
        
        if len(image_files) > 0:
            try:
                input_subdir = os.path.join(target_dir, "Input")
                os.makedirs(input_subdir)
                for img in image_files:
                    shutil.move(img, os.path.join(input_subdir, os.path.basename(img)))
                logger.info(f"Organized: Moved {len(image_files)} images in '{os.path.basename(target_dir)}' to 'Input/'")
            except Exception as e:
                logger.error(f"Failed to organize sequence in '{target_dir}': {e}")

    # Create Hints
    for hint in ["AlphaHint", "VideoMamaMaskHint"]:
        hint_path = os.path.join(target_dir, hint)
        if not os.path.exists(hint_path):
            os.makedirs(hint_path)
            # Create placeholder text file so git/user sees it? Optional.
            # with open(os.path.join(hint_path, "_place_sequence_here.txt"), 'w') as f:
            #     f.write("Place your image sequence here.")

def organize_clips(clips_dir):
    """
    Legacy wrapper for backward compatibility with 'ClipsForInference' folder.
    Organizes all subfolders in the given directory using the new logic.
    """
    if not os.path.exists(clips_dir):
        logger.warning(f"Clips directory not found: {clips_dir}")
        return

    logger.info(f"Organizing Clips Directory: {clips_dir}")
    
    # Check for loose videos in root
    loose_videos = [f for f in os.listdir(clips_dir) if is_video_file(f) and os.path.isfile(os.path.join(clips_dir, f))]
    
    # Organize loose videos first
    for v in loose_videos:
         clip_name = os.path.splitext(v)[0]
         ext = os.path.splitext(v)[1]
         target_folder = os.path.join(clips_dir, clip_name)
         
         if os.path.exists(target_folder):
             logger.warning(f"Skipping loose video '{v}': Target folder '{clip_name}' already exists.")
             continue
             
         try:
             os.makedirs(target_folder)
             target_file = os.path.join(target_folder, f"Input{ext}")
             shutil.move(os.path.join(clips_dir, v), target_file)
             logger.info(f"Organized: Moved '{v}' to '{clip_name}/Input{ext}'")
             
             # Also initialize hints immediately
             for hint in ["AlphaHint", "VideoMamaMaskHint"]:
                os.makedirs(os.path.join(target_folder, hint), exist_ok=True)
         except Exception as e:
             logger.error(f"Failed to organize video '{v}': {e}")
             
    # Now iterate all subdirectories and run organize_target
    for entry in os.listdir(clips_dir):
        full_path = os.path.join(clips_dir, entry)
        if os.path.isdir(full_path) and entry not in ["IgnoredClips", "Output"]:
            organize_target(full_path)

def interactive_wizard(win_path):
    print("\n" + "="*60)
    print(" CORRIDOR KEY - SMART WIZARD")
    print("="*60)
    
    # 1. Map Path
    linux_path = map_path(win_path)
    print(f"Windows Path: {win_path}")
    print(f"Linux Path:   {linux_path}")
    
    if not os.path.exists(linux_path):
        print(f"\n[ERROR] Path does not exist!")
        return

    # If user dragged a single video file, use its parent directory
    if os.path.isfile(linux_path) and is_video_file(linux_path):
        print(f"\nDetected single video file — using parent directory.")
        linux_path = os.path.dirname(linux_path)
        print(f"Working in: {linux_path}")
    elif os.path.isfile(linux_path):
        print(f"\n[ERROR] Path is a file, not a directory or video: {linux_path}")
        return

    # 2. Analyze
    # We treat linux_path as the ROOT containing SHOTS
    # Or is linux_path the SHOT itself?
    # HEURISTIC: If it contains "Input", it's a shot. If it contains folders, it's a project.
    
    # Let's assume it's a folder containing CLIPS (Batch Mode)
    # But if the user drops it IN a shot folder, we should handle that too.
    
    target_is_shot = False
    if os.path.exists(os.path.join(linux_path, "Input")) or glob.glob(os.path.join(linux_path, "Input.*")):
         target_is_shot = True
    
    work_dirs = []
    if target_is_shot:
        work_dirs = [linux_path]
    else:
        # Scan subfolders
        work_dirs = [os.path.join(linux_path, d) for d in os.listdir(linux_path) if os.path.isdir(os.path.join(linux_path, d))]
        # Filter out output/hints
        work_dirs = [d for d in work_dirs if os.path.basename(d) not in ["Output", "AlphaHint", "VideoMamaMaskHint", ".ipynb_checkpoints"]]
    
    print(f"\nFound {len(work_dirs)} potential clip folders.")
    
    # Check for loose videos in root
    loose_videos = [f for f in os.listdir(linux_path) if is_video_file(f) and os.path.isfile(os.path.join(linux_path, f))]
    
    # Check if existing folders need organization
    dirs_needing_org = []
    for d in work_dirs:
        # Check for Input
        has_input = os.path.exists(os.path.join(d, "Input")) or glob.glob(os.path.join(d, "Input.*"))
        # Check for hints
        has_alpha = os.path.exists(os.path.join(d, "AlphaHint"))
        has_mask = os.path.exists(os.path.join(d, "VideoMamaMaskHint"))
        
        if not has_input or not has_alpha or not has_mask:
            dirs_needing_org.append(d)

    if loose_videos or dirs_needing_org:
        if loose_videos:
            print(f"Found {len(loose_videos)} loose video files that need organization:")
            for v in loose_videos: print(f"  - {v}")
        
        if dirs_needing_org:
            print(f"Found {len(dirs_needing_org)} folders that might need setup (Hints/Input):")
            # Limit output if too many
            if len(dirs_needing_org) < 10:
                for d in dirs_needing_org: print(f"  - {os.path.basename(d)}")
            else:
                 print(f"  - ...and {len(dirs_needing_org)} others.")

        # 3. Organize Loop
        yn = input("\n[1] Organize Clips & Create Hint Folders? [y/N]: ").strip().lower()
        if yn == 'y':
            # Organize loose videos first
            for v in loose_videos:
                 clip_name = os.path.splitext(v)[0]
                 ext = os.path.splitext(v)[1]
                 target_folder = os.path.join(linux_path, clip_name)
                 
                 if os.path.exists(target_folder):
                     logger.warning(f"Skipping loose video '{v}': Target folder '{clip_name}' already exists.")
                     continue
                     
                 try:
                     os.makedirs(target_folder)
                     target_file = os.path.join(target_folder, f"Input{ext}")
                     shutil.move(os.path.join(linux_path, v), target_file)
                     logger.info(f"Organized: Moved '{v}' to '{clip_name}/Input{ext}'")
                     
                     # Also initialize hints immediately
                     for hint in ["AlphaHint", "VideoMamaMaskHint"]:
                        os.makedirs(os.path.join(target_folder, hint), exist_ok=True)
                 except Exception as e:
                     logger.error(f"Failed to organize video '{v}': {e}")
        
            # Organize existing folders
            for d in work_dirs:
                organize_target(d)
            print("Organization Complete.")
            
            # Re-scan in case structure changed
            # If it was a shot, it's still a shot (unless we messed it up)
            # If it wasn't a shot, we re-scan subdirs
            if not target_is_shot:
                 work_dirs = [os.path.join(linux_path, d) for d in os.listdir(linux_path) if os.path.isdir(os.path.join(linux_path, d))]
                 work_dirs = [d for d in work_dirs if os.path.basename(d) not in ["Output", "AlphaHint", "VideoMamaMaskHint"]]

    # 4. Status Check Loop
    while True:
        ready = []
        masked = []
        raw = []
        
        for d in work_dirs:
            entry = ClipEntry(os.path.basename(d), d)
            try:
                entry.find_assets() # This checks Input and AlphaHint
            except:
                pass # Might act up if Input missing
            
            # Check VideoMamaMaskHint (Strict: videomamamaskhint.ext or VideoMamaMaskHint/)
            has_mask = False
            mask_dir = os.path.join(d, "VideoMamaMaskHint")
            
            # 1. Directory Check
            if os.path.isdir(mask_dir) and len(os.listdir(mask_dir)) > 0:
                has_mask = True
            
            # 2. File Check (Strict Stem Match)
            if not has_mask:
                for f in os.listdir(d):
                    stem, _ = os.path.splitext(f)
                    if stem.lower() == "videomamamaskhint" and is_video_file(f):
                        has_mask = True
                        break
                
            if entry.alpha_asset:
                ready.append(entry)
            elif has_mask:
                masked.append(entry)
            else:
                raw.append(entry)
                
        print("\n" + "-"*40)
        print(f"STATUS REPORT:")
        print(f"  READY (AlphaHint found): {len(ready)}")
        for c in ready: print(f"    - {c.name}")
        
        print(f"  MASKED (VideoMamaMaskHint found): {len(masked)}")
        for c in masked: print(f"    - {c.name}")
        
        print(f"  RAW (Input only):        {len(raw)}")
        for c in raw: print(f"    - {c.name}")
        print("-"*40 + "\n")
        
        # Combine checks for actions
        missing_alpha = masked + raw
        total_missing = len(missing_alpha)
        
        print("\nACTIONS:")
        if missing_alpha:
            print(f"  [v] Run VideoMaMa (Found {len(masked)} ready with masks)")
            print(f"  [g] Run GVM (Auto-Matte on {len(raw)} clips without Mask Hint)")
        
        if ready:
            print(f"  [i] Run Inference (on {len(ready)} ready clips)")
            
        print("  [r] Re-Scan Folders")
        print("  [q] Quit")
        
        choice = input("\nSelect Action: ").strip().lower()
        
        if choice == 'v':
            # VideoMaMa
            print("\n--- VideoMaMa ---")
            print("Scanning for VideoMamaMaskHints...")
            # We pass ALL missing alpha clips. run_videomama checks for the actual files.
            run_videomama(missing_alpha, chunk_size=50)
            input("VideoMaMa batch complete. Press Enter to Re-Scan...")
            continue

        elif choice == 'g':
            # GVM
            print("\n--- GVM Auto-Matte ---")
            print(f"This will generate alphas for {len(raw)} clips that have NO Mask Hint.")
            
            yn = input("Proceed with GVM? [y/N]: ").strip().lower()
            if yn == 'y':
                generate_alphas(raw)
                input("GVM batch complete. Press Enter to Re-Scan...")
            continue
            
        elif choice == 'i':
            # Inference
            print("\n--- Corridor Key Inference ---")
            run_inference(ready)
            input("Inference batch complete. Press Enter to Re-Scan...")
            continue
            
        elif choice == 'r':
            print("\nRe-scanning...")
            continue
            
        elif choice == 'q':
            break
            
        else:
            print("Invalid selection.")
            continue

    print("\nWizard Complete. Goodbye!")


def scan_clips():
    if not os.path.exists(CLIPS_DIR):
        os.makedirs(CLIPS_DIR, exist_ok=True)
        return []

    # Auto-organize first
    organize_clips(CLIPS_DIR)

    clip_dirs = [d for d in os.listdir(CLIPS_DIR) if os.path.isdir(os.path.join(CLIPS_DIR, d))]
    
    valid_clips = []
    invalid_clips = []

    for d in clip_dirs:
        if d.startswith('.') or d.startswith('_') or d == "IgnoredClips": 
            continue
            
        full_path = os.path.join(CLIPS_DIR, d)
        
        try:
            entry = ClipEntry(d, full_path)
            entry.find_assets()
            entry.validate_pair()
            valid_clips.append(entry)
        except ValueError as ve:
            invalid_clips.append((d, str(ve)))
        except Exception as e:
             invalid_clips.append((d, f"Unexpected error: {e}"))

    if invalid_clips:
        print("\n" + "="*60)
        print(" INVALID OR SKIPPED CLIPS")
        print("="*60)
        for name, reason in invalid_clips:
            print(f"- {name}: {reason}")
        print("="*60 + "\n")
    else:
        print("\nAll clip folders appear valid.\n")

    return valid_clips

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="CorridorKey Clip Manager")
    parser.add_argument("--action", choices=['generate_alphas', 'run_inference', 'list', 'wizard'], required=True)
    parser.add_argument("--win_path", help=r"Windows Path (example: V:\...) for Wizard Mode", default=None)
    parser.add_argument("--non-interactive", action="store_true", help="Skip inference prompts and use CorridorKey 2.0 defaults")
    
    args = parser.parse_args()
    
    if args.action == 'list':
        scan_clips()
    elif args.action == 'generate_alphas':
        clips = scan_clips()
        generate_alphas(clips)
    elif args.action == 'run_inference':
        clips = scan_clips()
        sys.exit(1 if run_inference(clips, interactive=not args.non_interactive) else 0)
    elif args.action == 'wizard':
        if not args.win_path:
            print("Error: --win_path required for wizard.")
        else:
            interactive_wizard(args.win_path)
