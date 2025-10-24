//===== Copyright � 1996-2005, Valve Corporation, All rights reserved. ======//
//
// Purpose:
//
// $NoKeywords: $
//
//===========================================================================//

#include "shaderapimtl.h"

#include "materialsystem/idebugtextureinfo.h"

class CShaderAPIDx8 : public CShaderDeviceMTL, public IShaderAPIMTL, public IDebugTextureInfo
{
	typedef CShaderDeviceMTL BaseClass;

public:
	// constructor, destructor
	CShaderAPIDx8( );
	virtual ~CShaderAPIDx8();

	// Methods of IShaderAPI
public:
	virtual void SetViewports( int nCount, const ShaderViewport_t* pViewports, bool setImmediately = false );
	virtual int  GetViewports( ShaderViewport_t* pViewports, int nMax ) const;
	virtual void ClearBuffers( bool bClearColor, bool bClearDepth, bool bClearStencil, int renderTargetWidth, int renderTargetHeight );
	virtual void ClearColor3ub( unsigned char r, unsigned char g, unsigned char b );
	virtual void ClearColor4ub( unsigned char r, unsigned char g, unsigned char b, unsigned char a );
	virtual void BindVertexShader( VertexShaderHandle_t hVertexShader );
	virtual void BindGeometryShader( GeometryShaderHandle_t hGeometryShader );
	virtual void BindPixelShader( PixelShaderHandle_t hPixelShader );
	virtual void SetRasterState( const ShaderRasterState_t& state );
	virtual void SetFlexWeights( int nFirstWeight, int nCount, const MorphWeight_t* pWeights );
	virtual void OnPresent( void );
#ifdef _GAMECONSOLE
	// Backdoor used by the queued context to directly use write-combined memory
	virtual IMesh *GetExternalMesh( const ExternalMeshInfo_t& info );
	virtual void SetExternalMeshData( IMesh *pMesh, const ExternalMeshData_t &data );
	virtual IIndexBuffer *GetExternalIndexBuffer( int nIndexCount, uint16 *pIndexData );
	virtual void FlushGPUCache( void *pBaseAddr, size_t nSizeInBytes );
#endif

	// Methods of IShaderDynamicAPI
public:
	virtual void GetBackBufferDimensions( int &nWidth, int &nHeight ) const
	{
		// Chain to the device
		BaseClass::GetBackBufferDimensions( nWidth, nHeight );
	}

	virtual const AspectRatioInfo_t &GetAspectRatioInfo( void ) const
	{
		// Chain to the device
		return BaseClass::GetAspectRatioInfo();
	}

	// Get the dimensions of the current render target
	virtual void GetCurrentRenderTargetDimensions( int& nWidth, int& nHeight ) const
	{
		ITexture *pTexture = ShaderAPI()->GetRenderTargetEx( 0 );
		if ( pTexture == NULL )
		{
			ShaderAPI()->GetBackBufferDimensions( nWidth, nHeight );
		}
		else
		{
			nWidth  = pTexture->GetActualWidth();
			nHeight = pTexture->GetActualHeight();
		}
	}

	// Get the current viewport
	virtual void GetCurrentViewport( int& nX, int& nY, int& nWidth, int& nHeight ) const
	{
		ShaderViewport_t viewport;
		ShaderAPI()->GetViewports( &viewport, 1 );
		nX = viewport.m_nTopLeftX;
		nY = viewport.m_nTopLeftY;
		nWidth = viewport.m_nWidth;
		nHeight = viewport.m_nHeight;
	}

	virtual void MarkUnusedVertexFields( unsigned int nFlags, int nTexCoordCount, bool *pUnusedTexCoords );
	virtual void SetScreenSizeForVPOS( int pshReg = 32 );
	virtual void SetVSNearAndFarZ( int vshReg );
	virtual float GetFarZ();

	virtual void EnableSinglePassFlashlightMode( bool bEnable );
	virtual bool SinglePassFlashlightModeEnabled( void );

public:
	// Methods of CShaderAPIBase
	virtual bool OnDeviceInit();
	virtual void OnDeviceShutdown();
	virtual void ReleaseShaderObjects( bool bReleaseManagedResources = true );
	virtual void RestoreShaderObjects();
	virtual void BeginPIXEvent( unsigned long color, const char *szName );
	virtual void EndPIXEvent();
	virtual void AdvancePIXFrame();
	virtual void NotifyShaderConstantsChangedInRenderPass();
	virtual void GenerateNonInstanceRenderState( MeshInstanceData_t *pInstance, CompiledLightingState_t** pCompiledState, InstanceInfo_t **pInstanceInfo );

public:
	// Methods of IShaderAPIDX8
	virtual void QueueResetRenderState();

	//
	// Abandon all hope ye who pass below this line which hasn't been ported.
	//

	// Sets the mode...
	bool SetMode( void* VD3DHWND, int nAdapter, const ShaderDeviceInfo_t &info );

	// Change the video mode after it's already been set.
	void ChangeVideoMode( const ShaderDeviceInfo_t &info );

	// Sets the default render state
	void SetDefaultState();

	// Methods to ask about particular state snapshots
	virtual bool IsTranslucent( StateSnapshot_t id ) const;
	virtual bool IsAlphaTested( StateSnapshot_t id ) const;
	virtual bool UsesVertexAndPixelShaders( StateSnapshot_t id ) const;
	virtual int CompareSnapshots( StateSnapshot_t snapshot0, StateSnapshot_t snapshot1 );

	// Computes the vertex format for a particular set of snapshot ids
	VertexFormat_t ComputeVertexFormat( int num, StateSnapshot_t* pIds ) const;
	VertexFormat_t ComputeVertexUsage( int num, StateSnapshot_t* pIds ) const;

	// Uses a state snapshot
	void UseSnapshot( StateSnapshot_t snapshot );

	// Set the number of bone weights
	virtual void SetNumBoneWeights( int numBones );
	virtual void EnableHWMorphing( bool bEnable );

	// Sets the vertex and pixel shaders
	virtual void SetVertexShaderIndex( int vshIndex = -1 );
	virtual void SetPixelShaderIndex( int pshIndex = 0 );

	// Matrix state
	void MatrixMode( MaterialMatrixMode_t matrixMode );
	void PushMatrix();
	void PopMatrix();
	void LoadMatrix( float *m );
	void LoadBoneMatrix( int boneIndex, const float *m );
	void MultMatrix( float *m );
	void MultMatrixLocal( float *m );
	void GetMatrix( MaterialMatrixMode_t matrixMode, float *dst );
	void LoadIdentity( void );
	void LoadCameraToWorld( void );
	void Ortho( double left, double top, double right, double bottom, double zNear, double zFar );
	void PerspectiveX( double fovx, double aspect, double zNear, double zFar );
	void PerspectiveOffCenterX( double fovx, double aspect, double zNear, double zFar, double bottom, double top, double left, double right );
	void PickMatrix( int x, int y, int width, int height );
	void Rotate( float angle, float x, float y, float z );
	void Translate( float x, float y, float z );
	void Scale( float x, float y, float z );
	void ScaleXY( float x, float y );

	// Binds a particular material to render with
	void Bind( IMaterial* pMaterial );
	IMaterialInternal* GetBoundMaterial();

	// Level of anisotropic filtering
	virtual void SetAnisotropicLevel( int nAnisotropyLevel );

	virtual void	SyncToken( const char *pToken );

	void CullMode( MaterialCullMode_t cullMode );
	void FlipCullMode( void );

	// Force writes only when z matches. . . useful for stenciling things out
	// by rendering the desired Z values ahead of time.
	void ForceDepthFuncEquals( bool bEnable );

	// Turns off Z buffering
	void OverrideDepthEnable( bool bEnable, bool bDepthWriteEnable, bool bDepthTestEnable = true );

	void OverrideAlphaWriteEnable( bool bOverrideEnable, bool bAlphaWriteEnable );
	void OverrideColorWriteEnable( bool bOverrideEnable, bool bColorWriteEnable );

	void SetHeightClipZ( float z );
	void SetHeightClipMode( enum MaterialHeightClipMode_t heightClipMode );

	void SetClipPlane( int index, const float *pPlane );
	void EnableClipPlane( int index, bool bEnable );

	void SetFastClipPlane(const float *pPlane);
	void EnableFastClip(bool bEnable);

	// The shade mode
	void ShadeMode( ShaderShadeMode_t mode );

	// Gets the dynamic mesh
	IMesh* GetDynamicMesh( IMaterial* pMaterial, int nHWSkinBoneCount, bool buffered,
		IMesh* pVertexOverride, IMesh* pIndexOverride );
	IMesh* GetDynamicMeshEx( IMaterial* pMaterial, VertexFormat_t vertexFormat, int nHWSkinBoneCount,
		bool bBuffered, IMesh* pVertexOverride, IMesh* pIndexOverride );
	IMesh *GetFlexMesh();

	// Returns the number of vertices we can render using the dynamic mesh
	virtual void GetMaxToRender( IMesh *pMesh, bool bMaxUntilFlush, int *pMaxVerts, int *pMaxIndices );
	virtual int GetMaxVerticesToRender( IMaterial *pMaterial );
	virtual int GetMaxIndicesToRender( );

	// Draws the mesh
	void DrawMesh( CMeshBase* mesh, int nCount, const MeshInstanceData_t *pInstances, VertexCompressionType_t nCompressionType, CompiledLightingState_t* pCompiledState, InstanceInfo_t *pInfo );
	void DrawMesh2( CMeshBase* mesh, int nCount, const MeshInstanceData_t *pInstances, VertexCompressionType_t nCompressionType, CompiledLightingState_t* pCompiledState, InstanceInfo_t *pInfo );
	void DrawShadowMesh( CMeshBase* mesh, int nCount, const MeshInstanceData_t *pInstances, VertexCompressionType_t nCompressionType, CompiledLightingState_t* pCompiledState, InstanceInfo_t *pInfo );
	void DrawMeshInternal( CMeshBase* mesh, int nCount, const MeshInstanceData_t *pInstances, VertexCompressionType_t nCompressionType, CompiledLightingState_t* pCompiledState, InstanceInfo_t *pInfo );

	// Draws
	void BeginPass( StateSnapshot_t snapshot  );
	void RenderPass( const unsigned char *pInstanceCommandBuffer, int nPass, int nPassCount );

	// We use smaller dynamic VBs during level transitions, to free up memory
	virtual int  GetCurrentDynamicVBSize( void );
	virtual void DestroyVertexBuffers( bool bExitingLevel = false );

	void SetVertexDecl( VertexFormat_t vertexFormat, bool bHasColorMesh, bool bUsingFlex, bool bUsingMorph, bool bUsingPreTessPatch, VertexStreamSpec_t *pStreamSpec );

	void SetTessellationMode( TessellationMode_t mode );

	// Sets the constant register for vertex and pixel shaders
	FORCEINLINE void SetVertexShaderConstantInternal( int var, float const* pVec, int numVecs = 1, bool bForce = false );

	void SetVertexShaderConstant( int var, float const* pVec, int numVecs = 1, bool bForce = false );
	void SetBooleanVertexShaderConstant( int var, BOOL const* pVec, int numBools = 1, bool bForce = false );
	void SetIntegerVertexShaderConstant( int var, int const* pVec, int numIntVecs = 1, bool bForce = false );

	void SetPixelShaderConstant( int var, float const* pVec, int numVecs = 1, bool bForce = false );
#ifdef _PS3
	FORCEINLINE void SetPixelShaderConstantInternal( int var, float const* pValues, int nNumConsts = 1, bool bForce = false )
	{
		Dx9Device()->SetPixelShaderConstantF( var, pValues, nNumConsts );
	}
#else
	FORCEINLINE void SetPixelShaderConstantInternal( int var, float const* pValues, int nNumConsts = 1, bool bForce = false );
#endif

	void SetBooleanPixelShaderConstant( int var, BOOL const* pVec, int numBools = 1, bool bForce = false );
	void SetIntegerPixelShaderConstant( int var, int const* pVec, int numIntVecs = 1, bool bForce = false );

	void InvalidateDelayedShaderConstants( void );

	// Returns the nearest supported format
	ImageFormat GetNearestSupportedFormat( ImageFormat fmt, bool bFilteringRequired = true ) const;
	ImageFormat GetNearestRenderTargetFormat( ImageFormat format ) const;
	virtual bool DoRenderTargetsNeedSeparateDepthBuffer() const;

	// stuff that shouldn't be used from within a shader
    void ModifyTexture( ShaderAPITextureHandle_t textureHandle );
	void BindTexture( Sampler_t sampler, TextureBindFlags_t nBindFlags, ShaderAPITextureHandle_t textureHandle );
	void BindVertexTexture( VertexTextureSampler_t nStage, ShaderAPITextureHandle_t textureHandle );
	void DeleteTexture( ShaderAPITextureHandle_t textureHandle );

	void WriteTextureToFile( ShaderAPITextureHandle_t hTexture, const char *szFileName );

	bool IsTexture( ShaderAPITextureHandle_t textureHandle );
	bool IsTextureResident( ShaderAPITextureHandle_t textureHandle );
	FORCEINLINE bool TextureIsAllocated( ShaderAPITextureHandle_t hTexture )
	{
		return m_Textures.IsValidIndex( hTexture ) && ( GetTexture( hTexture ).m_Flags & Texture_t::IS_ALLOCATED );
	}
	FORCEINLINE void AssertValidTextureHandle( ShaderAPITextureHandle_t textureHandle )
	{
#ifdef _DEBUG
		Assert( TextureIsAllocated( textureHandle ) );
#endif
	}

	// Lets the shader know about the full-screen texture so it can
	virtual void SetFullScreenTextureHandle( ShaderAPITextureHandle_t h );

	virtual void SetLinearToGammaConversionTextures( ShaderAPITextureHandle_t hSRGBWriteEnabledTexture, ShaderAPITextureHandle_t hIdentityTexture );

	// Set the render target to a texID.
	// Set to SHADER_RENDERTARGET_BACKBUFFER if you want to use the regular framebuffer.
	void SetRenderTarget( ShaderAPITextureHandle_t colorTextureHandle = SHADER_RENDERTARGET_BACKBUFFER,
		ShaderAPITextureHandle_t depthTextureHandle = SHADER_RENDERTARGET_DEPTHBUFFER );
	// Set the render target to a texID.
	// Set to SHADER_RENDERTARGET_BACKBUFFER if you want to use the regular framebuffer.
	void SetRenderTargetEx( int nRenderTargetID, ShaderAPITextureHandle_t colorTextureHandle = SHADER_RENDERTARGET_BACKBUFFER,
		ShaderAPITextureHandle_t depthTextureHandle = SHADER_RENDERTARGET_DEPTHBUFFER );

	// These are bound to the texture, not the texture environment
	void TexMinFilter( ShaderTexFilterMode_t texFilterMode );
	void TexMagFilter( ShaderTexFilterMode_t texFilterMode );
	void TexWrap( ShaderTexCoordComponent_t coord, ShaderTexWrapMode_t wrapMode );
	void TexSetPriority( int priority );

	ShaderAPITextureHandle_t CreateTextureHandle( void );
	void CreateTextureHandles( ShaderAPITextureHandle_t *handles, int count, bool bReuseHandles );

	ShaderAPITextureHandle_t CreateTexture(
		int width,
		int height,
		int depth,
		ImageFormat dstImageFormat,
		int numMipLevels,
		int numCopies,
		int creationFlags,
		const char *pDebugName,
		const char *pTextureGroupName );

	// Create a multi-frame texture (equivalent to calling "CreateTexture" multiple times, but more efficient)
	void CreateTextures(
		ShaderAPITextureHandle_t *pHandles,
		int count,
		int width,
		int height,
		int depth,
		ImageFormat dstImageFormat,
		int numMipLevels,
		int numCopies,
		int flags,
		const char *pDebugName,
		const char *pTextureGroupName );

	ShaderAPITextureHandle_t CreateDepthTexture(
		ImageFormat renderTargetFormat,
		int width,
		int height,
		const char *pDebugName,
		bool bTexture,
		bool bAliasDepthSurfaceOverColorX360 = false );

	void TexImage2D(
		int level,
		int cubeFaceID,
		ImageFormat dstFormat,
		int zOffset,
		int width,
		int height,
		ImageFormat srcFormat,
		bool bSrcIsTiled,
		void *imageData );

	void TexSubImage2D(
		int level,
		int cubeFaceID,
		int xOffset,
		int yOffset,
		int zOffset,
		int width,
		int height,
		ImageFormat srcFormat,
		int srcStride,
		bool bSrcIsTiled,
		void *imageData );

	bool TexLock( int level, int cubeFaceID, int xOffset, int yOffset, int width, int height, CPixelWriter& writer );
	void TexUnlock( );

	// Copy sysmem surface to default pool surface asynchronously
	void UpdateTexture( int xOffset, int yOffset, int w, int h, ShaderAPITextureHandle_t hDstTexture, ShaderAPITextureHandle_t hSrcTexture );

	void *LockTex( ShaderAPITextureHandle_t hTexture );
	void UnlockTex( ShaderAPITextureHandle_t hTexture );

	void UpdateDepthBiasState();

	// stuff that isn't to be used from within a shader
	// what's the best way to hide this? subclassing?
	virtual void ClearBuffersObeyStencil( bool bClearColor, bool bClearDepth );
	virtual void ClearBuffersObeyStencilEx( bool bClearColor, bool bClearAlpha, bool bClearDepth );
	virtual void PerformFullScreenStencilOperation( void );
	void ReadPixels( int x, int y, int width, int height, unsigned char *data, ImageFormat dstFormat, ITexture *pRenderTargetTexture = NULL );
	void ReadPixelsAsync( int x, int y, int width, int height, unsigned char *data, ImageFormat dstFormat, ITexture *pRenderTargetTexture = NULL, CThreadEvent *pPixelsReadEvent = NULL );
	void ReadPixelsAsyncGetResult( int x, int y, int width, int height, unsigned char *data, ImageFormat dstFormat, CThreadEvent *pGetResultEvent = NULL );
	virtual void ReadPixels( Rect_t *pSrcRect, Rect_t *pDstRect, unsigned char *data, ImageFormat dstFormat, int nDstStride );

	// Gets the current buffered state... (debug only)
	void GetBufferedState( BufferedState_t& state );

	// Make sure we finish drawing everything that has been requested
	void FlushHardware();

	// Use this to begin and end the frame
	void BeginFrame();
	void EndFrame();

	// Used to clear the transition table when we know it's become invalid.
	void ClearSnapshots();

	// returns the D3D interfaces....
#if !defined( _GAMECONSOLE )
	FORCEINLINE D3DDeviceWrapper *Dx9Device() const
	{
		return (D3DDeviceWrapper*)&(m_DeviceWrapper);
	}
#endif

	// Backward compat
	virtual int GetActualSamplerCount() const;
	virtual int StencilBufferBits() const;
	virtual bool IsAAEnabled() const;	// Is antialiasing being used?
	virtual bool OnAdapterSet( );
	bool m_bAdapterSet;

	void UpdateFastClipUserClipPlane( void );
	bool ReadPixelsFromFrontBuffer() const;

	// returns the current time in seconds....
	double CurrentTime() const;

	// Get the current camera position in world space.
	void GetWorldSpaceCameraPosition( float* pPos ) const;
	void GetWorldSpaceCameraDirection( float* pDir ) const;

	// Fog methods
	void EnableFixedFunctionFog( bool bFogEnable );
	void FogStart( float fStart );
	void FogEnd( float fEnd );
	void FogMaxDensity( float flMaxDensity );
	void SetFogZ( float fogZ );
	void GetFogDistances( float *fStart, float *fEnd, float *fFogZ );

	void SceneFogMode( MaterialFogMode_t fogMode );
	MaterialFogMode_t GetSceneFogMode( );
	int GetPixelFogCombo( );//0 is either range fog, or no fog simulated with rigged range fog values. 1 is height fog
	void SceneFogColor3ub( unsigned char r, unsigned char g, unsigned char b );
	void GetSceneFogColor( unsigned char *rgb );
	void GetSceneFogColor( unsigned char *r, unsigned char *g, unsigned char *b );


	/// update the array of precalculated color values when state changes.
	void RegenerateFogConstants( void );

	Vector CalculateFogColorConstant( D3DCOLOR *pPackedColorOut, FogMethod_t fogMethod, ShaderFogMode_t fogMode,
									  bool bDisableFogGammaCorrection, bool bSRGBWritesEnabled );


	// Selection mode methods
	int  SelectionMode( bool selectionMode );
	void SelectionBuffer( unsigned int* pBuffer, int size );
	void ClearSelectionNames( );
	void LoadSelectionName( int name );
	void PushSelectionName( int name );
	void PopSelectionName();
	bool IsInSelectionMode() const;
	void RegisterSelectionHit( float minz, float maxz );
	void WriteHitRecord();

	// Binds a standard texture
	virtual void BindStandardTexture( Sampler_t sampler, TextureBindFlags_t nBindFlags, StandardTextureId_t id );
	virtual void BindStandardVertexTexture( VertexTextureSampler_t sampler, StandardTextureId_t id );
	virtual void GetStandardTextureDimensions( int *pWidth, int *pHeight, StandardTextureId_t id );

	virtual float GetSubDHeight();

	virtual bool IsStereoActiveThisFrame() const;
	bool m_bIsStereoActiveThisFrame;

	// Gets the lightmap dimensions
	virtual void GetLightmapDimensions( int *w, int *h );

	// Use this to get the mesh builder that allows us to modify vertex data
	CMeshBuilder* GetVertexModifyBuilder();

	virtual bool InFlashlightMode() const;
	virtual bool IsRenderingPaint() const;
	virtual bool InEditorMode() const;

	// Helper to get at the texture state stage
	SamplerState_t& SamplerState( int nSampler ) { return m_DynamicState.m_SamplerState[nSampler]; }
	const SamplerState_t& SamplerState( int nSampler ) const { return m_DynamicState.m_SamplerState[nSampler]; }
	TextureBindFlags_t LastSetTextureBindFlags( int nSampler ) const { return SamplerState( nSampler ).m_nTextureBindFlags; }
	void SetLastSetTextureBindFlags( int nSampler, TextureBindFlags_t nBindFlags ) { SamplerState( nSampler ).m_nTextureBindFlags = nBindFlags; }

	virtual void SetLightingState( const MaterialLightingState_t& state );
	void SetLights( int nCount, const LightDesc_t *pDesc );
	void SetLightingOrigin( Vector vLightingOrigin );
	void DisableAllLocalLights();
	void SetAmbientLightCube( Vector4D colors[6] );
	float GetAmbientLightCubeLuminance( MaterialLightingState_t *pLightingState );

	void SetVertexShaderStateAmbientLightCube( int nVSReg, CompiledLightingState_t *pLightingState );
	void SetPixelShaderStateAmbientLightCube( int pshReg, CompiledLightingState_t *pLightingState );

	// Methods related to compiling lighting state for use in shaderes
	void CompileAmbientCube( CompiledLightingState_t *pCompiledState, int nLightCount, const MaterialLightingState_t *pLightingState );
	void CompileVertexShaderLocalLights( CompiledLightingState_t *pCompiledState, int nLightCount, const MaterialLightingState_t *pLightingState, bool bStaticLight );
	void CompilePixelShaderLocalLights( CompiledLightingState_t *pCompiledState, int nLightCount, const MaterialLightingState_t *pLightingState, bool bStaticLight );

	void CopyRenderTargetToTexture( ShaderAPITextureHandle_t textureHandle );
	void CopyRenderTargetToTextureEx( ShaderAPITextureHandle_t textureHandle, int nRenderTargetID, Rect_t *pSrcRect = NULL, Rect_t *pDstRect = NULL );
	void CopyTextureToRenderTargetEx( int nRenderTargetID, ShaderAPITextureHandle_t textureHandle, Rect_t *pSrcRect = NULL, Rect_t *pDstRect = NULL );

	// Returns the cull mode (for fill rate computation)
	D3DCULL GetCullMode() const;
	void SetCullModeState( bool bEnable, D3DCULL nDesiredCullMode );
	void ApplyCullEnable( bool bEnable );
	void FlipCulling( bool bFlipCulling );

	// Alpha to coverage
	void ApplyAlphaToCoverage( bool bEnable );

	// Applies Z Bias
	void ApplyZBias( const DepthTestState_t &shaderState );

	// Applies texture enable
	void ApplyTextureEnable( const ShadowState_t& state, int stage );

	void ApplyFogMode( ShaderFogMode_t fogMode, bool bVertexFog, bool bSRGBWritesEnabled, bool bDisableFogGammaCorrection );

	void UpdatePixelFogColorConstant( bool bMultiplyByToneMapScale = true );

	void EnabledSRGBWrite( bool bEnabled );
	void SetSRGBWrite( bool bState );

	// Gamma<->Linear conversions according to the video hardware we're running on
	float GammaToLinear_HardwareSpecific( float fGamma ) const;
	float LinearToGamma_HardwareSpecific( float fLinear ) const;

	// Applies alpha blending
	void ApplyAlphaBlend( bool bEnable, D3DBLEND srcBlend, D3DBLEND destBlend );

	// Sets texture stage stage + render stage state
	void SetSamplerState( int stage, D3DSAMPLERSTATETYPE state, DWORD val );
	void SetRenderStateForce( D3DRENDERSTATETYPE state, DWORD val );
	void SetRenderState( D3DRENDERSTATETYPE state, DWORD val );

	// Scissor Rect
	void SetScissorRect( const int nLeft, const int nTop, const int nRight, const int nBottom, const bool bEnableScissor );
	// Can we download textures?
	virtual bool CanDownloadTextures() const;

	void ForceHardwareSync_WithManagedTexture();
	void ForceHardwareSync( void );
	void UpdateFrameSyncQuery( int queryIndex, bool bIssue );

	void EvictManagedResources();

	// Get stats on GPU memory usage
	virtual void GetGPUMemoryStats( GPUMemoryStats &stats );

	virtual void EvictManagedResourcesInternal();

	// Gets at a particular transform
	inline D3DXMATRIX& GetTransform( int i )
	{
		return *m_pMatrixStack[i]->GetTop();
	}

	int GetCurrentNumBones( void ) const;
	bool IsHWMorphingEnabled( ) const;
	TessellationMode_t GetTessellationMode( ) const;
	void GetDX9LightState( LightState_t *state ) const;	// Used for DX9 only

	MaterialFogMode_t GetCurrentFogType( void ) const; // deprecated. don't use.

	void RecordString( const char *pStr );

	virtual bool IsRenderingMesh() const { return m_nRenderInstanceCount != 0; }

	int GetCurrentFrameCounter( void ) const
	{
		return m_CurrentFrame;
	}

	// Workaround hack for visualization of selection mode
	virtual void SetupSelectionModeVisualizationState();

	// Allocate and delete query objects.
	virtual ShaderAPIOcclusionQuery_t CreateOcclusionQueryObject( void );
	virtual void DestroyOcclusionQueryObject( ShaderAPIOcclusionQuery_t h );

	// Bracket drawing with begin and end so that we can get counts next frame.
	virtual void BeginOcclusionQueryDrawing( ShaderAPIOcclusionQuery_t h );
	virtual void EndOcclusionQueryDrawing( ShaderAPIOcclusionQuery_t h );

	// Get the number of pixels rendered between begin and end on an earlier frame.
	// Calling this in the same frame is a huge perf hit!
	virtual int OcclusionQuery_GetNumPixelsRendered( ShaderAPIOcclusionQuery_t h, bool bFlush );

	void SetFlashlightState( const FlashlightState_t &state, const VMatrix &worldToTexture );
	void SetFlashlightStateEx( const FlashlightState_t &state, const VMatrix &worldToTexture, ITexture *pFlashlightDepthTexture );
	const FlashlightState_t &GetFlashlightState( VMatrix &worldToTexture ) const;
	const FlashlightState_t &GetFlashlightStateEx( VMatrix &worldToTexture, ITexture **pFlashlightDepthTexture ) const;
	virtual void GetFlashlightShaderInfo( bool *pShadowsEnabled, bool *pUberLight ) const;
	virtual float GetFlashlightAmbientOcclusion( ) const;

	virtual bool IsCascadedShadowMapping() const;
	virtual void SetCascadedShadowMappingState( const CascadedShadowMappingState_t &state, ITexture *pDepthTextureAtlas );
	virtual const CascadedShadowMappingState_t &GetCascadedShadowMappingState( ITexture **pDepthTextureAtlas, bool bLightMapScale = false ) const;

	// Gets at the shadow state for a particular state snapshot
	virtual bool IsDepthWriteEnabled( StateSnapshot_t id ) const;

// IDebugTextureInfo implementation.

	virtual bool IsDebugTextureListFresh( int numFramesAllowed = 1 );
	virtual void EnableDebugTextureList( bool bEnable );
	virtual bool SetDebugTextureRendering( bool bEnable );
	virtual void EnableGetAllTextures( bool bEnable );
	virtual KeyValues* LockDebugTextureList( void );
	virtual void UnlockDebugTextureList( void );
	virtual int GetTextureMemoryUsed( TextureMemoryType eTextureMemory );

	virtual void ClearVertexAndPixelShaderRefCounts();
	virtual void PurgeUnusedVertexAndPixelShaders();

	// Called when the dx support level has changed
	virtual void DXSupportLevelChanged( int nDXLevel );

	// User clip plane override
	virtual void EnableUserClipTransformOverride( bool bEnable );
	virtual void UserClipTransform( const VMatrix &worldToProjection );

	bool UsingSoftwareVertexProcessing() const;

	// Mark all user clip planes as being dirty
	void MarkAllUserClipPlanesDirty();

	// Converts a D3DXMatrix to a VMatrix and back
	void D3DXMatrixToVMatrix( const D3DXMATRIX &in, VMatrix &out );
	void VMatrixToD3DXMatrix( const VMatrix &in, D3DXMATRIX &out );

	ITexture *GetRenderTargetEx( int nRenderTargetID ) const;

	virtual void SetToneMappingScaleLinear( const Vector &scale );
	virtual const Vector &GetToneMappingScaleLinear( void ) const;

	void SetFloatRenderingParameter(int parm_number, float value);					// Rendering Parameter Setters
	void SetIntRenderingParameter(int parm_number, int value);						//
	void SetTextureRenderingParameter(int parm_number, ITexture *pTexture);			//
	void SetVectorRenderingParameter(int parm_number, Vector const &value);			//

	float GetFloatRenderingParameter(int parm_number) const;						// Rendering Parameter Getters
	int GetIntRenderingParameter(int parm_number) const;							//
	ITexture *GetTextureRenderingParameter(int parm_number) const;					//
	Vector GetVectorRenderingParameter(int parm_number) const;						//

	// For dealing with device lost in cases where Present isn't called all the time (Hammer)
	virtual void HandleDeviceLost();

	virtual void EnableLinearColorSpaceFrameBuffer( bool bEnable );

	void SetStencilState( const ShaderStencilState_t &state );												// Stencil methods
	void SetStencilStateInternal( const ShaderStencilState_t &state );
	void GetCurrentStencilState( ShaderStencilState_t *pState );
	void ClearStencilBufferRectangle(int xmin, int ymin, int xmax, int ymax,int value);

#if defined ( _GAMECONSOLE )
	bool PostQueuedTexture( const void *pData, int nSize, ShaderAPITextureHandle_t *pHandles, int nHandles, int nWidth, int nHeight, int nDepth, int nMips, int *pRefCount );
#endif

#if defined( _X360 )
	HXUIFONT OpenTrueTypeFont( const char *pFontname, int tall, int style );
	void CloseTrueTypeFont( HXUIFONT hFont );
	bool GetTrueTypeFontMetrics( HXUIFONT hFont, wchar_t wchFirst, wchar_t wchLast, XUIFontMetrics *pFontMetrics, XUICharMetrics *pCharMetrics );
	// Render a sequence of characters and extract the data into a buffer
	// For each character, provide the width+height of the font texture subrect,
	// an offset to apply when rendering the glyph, and an offset into a buffer to receive the RGBA data
	bool GetTrueTypeGlyphs( HXUIFONT hFont, int numChars, wchar_t *pWch, int *pOffsetX, int *pOffsetY, int *pWidth, int *pHeight, unsigned char *pRGBA, int *pRGBAOffset );
	ShaderAPITextureHandle_t CreateRenderTargetSurface( int width, int height, ImageFormat format, RTMultiSampleCount360_t multiSampleCount, const char *pDebugName, const char *pTextureGroupName );
	void PersistDisplay();
	void *GetD3DDevice();

	void PushVertexShaderGPRAllocation( int iVertexShaderCount = 64 );
	void PopVertexShaderGPRAllocation( void );

	void EnableVSync_360( bool bEnable );

	virtual void SetCacheableTextureParams( ShaderAPITextureHandle_t *pHandles, int count, const char *pFilename, int mipSkipCount );
	virtual void FlushHiStencil();
#endif

#if defined( _GAMECONSOLE )
	virtual void BeginConsoleZPass2( int nNumDynamicIndicesNeeded );
	virtual void EndConsoleZPass();
	virtual unsigned int GetConsoleZPassCounter() const { return m_nZPassCounter; }

	virtual void EnablePredication( bool bZPass, bool bRenderPass );
	virtual void DisablePredication();
#endif

#if defined( _PS3 )
	virtual void FlushTextureCache();
#endif
	virtual void AntiAliasingHint( int nHint );

	virtual bool OwnGPUResources( bool bEnable );

// ------------ New Vertex/Index Buffer interface ----------------------------
	void BindVertexBuffer( int streamID, IVertexBuffer *pVertexBuffer, int nOffsetInBytes, int nFirstVertex, int nVertexCount, VertexFormat_t fmt, int nRepetitions = 1 );
	void BindIndexBuffer( IIndexBuffer *pIndexBuffer, int nOffsetInBytes );
	void Draw( MaterialPrimitiveType_t primitiveType, int nFirstIndex, int nIndexCount );

	// Draw the mesh with the currently bound vertex and index buffers.
	void DrawWithVertexAndIndexBuffers( void );
// ------------ End ----------------------------

	// deformations
	virtual void PushDeformation( const DeformationBase_t *pDeformation );
	virtual void PopDeformation( );
	virtual int GetNumActiveDeformations( ) const ;


	// for shaders to set vertex shader constants. returns a packed state which can be used to set the dynamic combo
	virtual int GetPackedDeformationInformation( int nMaskOfUnderstoodDeformations,
												 float *pConstantValuesOut,
												 int nBufferSize,
												 int nMaximumDeformations,
												 int *pNumDefsOut ) const ;

	inline Texture_t &GetTexture( ShaderAPITextureHandle_t hTexture )
	{
		return m_Textures[hTexture];
	}

	// Gets the texture
	IDirect3DBaseTexture* GetD3DTexture( ShaderAPITextureHandle_t hTexture );
	virtual void* GetD3DTexturePtr( ShaderAPITextureHandle_t hTexture );
	virtual bool IsStandardTextureHandleValid( StandardTextureId_t textureId );

#ifdef _PS3
	virtual void GetPs3Texture(void* tex, ShaderAPITextureHandle_t hTexture );
	virtual void GetPs3Texture(void* tex, StandardTextureId_t nTextureId );
#endif

	virtual bool ShouldWriteDepthToDestAlpha( void ) const;

	virtual void AcquireThreadOwnership();
	virtual void ReleaseThreadOwnership();

	virtual void UpdateGameTime( float flTime ) { m_flCurrGameTime = flTime; }

	virtual bool IsStereoSupported() const;
	virtual void UpdateStereoTexture( ShaderAPITextureHandle_t texHandle, bool *pStereoActiveThisFrame );
#ifndef _PS3
private:
#endif
	enum
	{
		SMALL_BACK_BUFFER_SURFACE_WIDTH = 256,
		SMALL_BACK_BUFFER_SURFACE_HEIGHT = 256,
	};

	bool m_bEnableDebugTextureList;
	bool m_bDebugGetAllTextures;
	bool m_bDebugTexturesRendering;
	KeyValues *m_pDebugTextureList;
	CThreadFastMutex m_DebugTextureListLock;

	int m_nTextureMemoryUsedLastFrame, m_nTextureMemoryUsedTotal;
	int m_nTextureMemoryUsedPicMip1, m_nTextureMemoryUsedPicMip2;
	int m_nDebugDataExportFrame;

	FlashlightState_t m_FlashlightState;
	VMatrix m_FlashlightWorldToTexture;
	ITexture *m_pFlashlightDepthTexture;
	UberlightRenderState_t m_UberlightRenderState;
	float m_pFlashlightAtten[4];
	float m_pFlashlightPos[4];
	float m_pFlashlightColor[4];
	float m_pFlashlightTweaks[4];
	DepthBiasState_t m_ZBias;
	DepthBiasState_t m_ZBiasDecal;

	CascadedShadowMappingState_t m_CascadedShadowMappingState;
	CascadedShadowMappingState_t m_CascadedShadowMappingState_LightMapScaled;
	ITexture *m_pCascadedShadowMappingDepthTexture;

	CShaderAPIDx8( CShaderAPIDx8 const& );

	enum
	{
		INVALID_TRANSITION_OP = 0xFFFF
	};

	// State transition table for the device is as follows:

	// Other app init causes transition from OK to OtherAppInit, during transition we must release resources
	// !Other app init causes transition from OtherAppInit to OK, during transition we must restore resources
	// Minimized or device lost or device not reset causes transition from OK to LOST_DEVICE, during transition we must release resources
	// Minimized or device lost or device not reset causes transition from OtherAppInit to LOST_DEVICE

	// !minimized AND !device lost causes transition from LOST_DEVICE to NEEDS_RESET
	// minimized or device lost causes transition from NEEDS_RESET to LOST_DEVICE

	// Successful TryDeviceReset and !Other app init causes transition from NEEDS_RESET to OK, during transition we must restore resources
	// Successful TryDeviceReset and Other app init causes transition from NEEDS_RESET to OtherAppInit

	void ExportTextureList();
	void AddBufferToTextureList( const char *pName, D3DSURFACE_DESC &desc );
	int D3DFormatToBitsPerPixel( D3DFORMAT fmt ) const;

	void SetupTextureGroup( ShaderAPITextureHandle_t hTexture, const char *pTextureGroupName );

	// Creates the matrix stack
	void CreateMatrixStacks();

	// Initializes the render state
	void InitRenderState( );

	// Resets all dx renderstates to dx default so that our shadows are correct.
	void ResetDXRenderState( );

	// Resets the render state
	void ResetRenderState( bool bFullReset = true );

	// Setup standard vertex shader constants (that don't change)
	void SetStandardVertexShaderConstants( float fOverbright );

	// Initializes vertex and pixel shaders
	void InitVertexAndPixelShaders();

	// Discards the vertex and index buffers
	void DiscardVertexBuffers();

	// Computes the fill rate
	void ComputeFillRate();

	// Takes a snapshot
	virtual StateSnapshot_t TakeSnapshot( );

	// Converts the clear color to be appropriate for HDR
	D3DCOLOR GetActualClearColor( D3DCOLOR clearColor );

	// We lost the device
	void OnDeviceLost();

	// Gets the matrix stack from the matrix mode
	int GetMatrixStack( MaterialMatrixMode_t mode ) const;

	// Flushes the matrix state, returns false if we don't need to
	// do any more work
	bool MatrixIsChanging( TransformType_t transform = TRANSFORM_IS_GENERAL );

	// Updates the matrix transform state
	void UpdateMatrixTransform( TransformType_t transform = TRANSFORM_IS_GENERAL );

	// Sets the vertex shader modelView state..
	// NOTE: GetProjectionMatrix should only be called from the Commit functions!
	const D3DXMATRIX &GetProjectionMatrix( void );
	void SetVertexShaderViewProj();
	void SetVertexShaderModelViewProjAndModelView();

	void SetPixelShaderFogParams( int reg );
	void SetPixelShaderFogParams( int reg, ShaderFogMode_t fogMode );

	void SetVertexShaderCameraPos()
	{
		float vertexShaderCameraPos[4];
		vertexShaderCameraPos[0] = m_WorldSpaceCameraPosition[0];
		vertexShaderCameraPos[1] = m_WorldSpaceCameraPosition[1];
		vertexShaderCameraPos[2] = m_WorldSpaceCameraPosition[2];
		vertexShaderCameraPos[3] = m_DynamicState.m_FogZ;  //waterheight in water fog mode

		// eyepos.x eyepos.y eyepos.z cWaterZ
		SetVertexShaderConstantInternal( VERTEX_SHADER_CAMERA_POS, vertexShaderCameraPos );
	}

	// This is where vertex shader constants are set for both hardware vertex fog and pixel shader-blended vertex fog.
	FORCEINLINE void UpdateVertexShaderFogParams( ShaderFogMode_t fogMode = SHADER_FOGMODE_NUMFOGMODES, bool bVertexFog = false )
	{
		float fogParams[4];

		if( fogMode == SHADER_FOGMODE_NUMFOGMODES ) //not passing in an explicit fog mode
		{
			if ( m_TransitionTable.CurrentShadowState() == NULL )
			{
				fogMode = SHADER_FOGMODE_DISABLED;
				bVertexFog = false;
			}
			else
			{
				fogMode = ( ShaderFogMode_t ) m_TransitionTable.CurrentShadowState()->m_FogAndMiscState.m_FogMode;
				bVertexFog = m_TransitionTable.CurrentShadowState()->m_FogAndMiscState.m_bVertexFogEnable;
			}
		}

		if( (m_SceneFogMode != MATERIAL_FOG_NONE) && (fogMode != SHADER_FOGMODE_DISABLED) )
		{
			//prepare the values for Fixed Function fog, and we'll modify them to handle pixel fog if necessary
			float ooFogRange = 1.0f;

			float fStart = m_VertexShaderFogParams[0];
			float fEnd = m_VertexShaderFogParams[1];

			// Check for divide by zero
			if ( fEnd != fStart )
			{
				ooFogRange = 1.0f / ( fEnd - fStart );
			}

			// Fixed-function-blended per-vertex fog requires some inverted params since a fog factor of 0 means fully fogged and 1 means no fog.
			// We could implement shader fog the same way, but would require an extra subtract in the vertex and/or pixel shader, which we want to avoid.
			if ( HardwareConfig()->GetDXSupportLevel() <= 90 )
			{
				fogParams[0] = ooFogRange * fEnd;
				fogParams[1] = 0.0f;
				fogParams[2] = 1.0f - clamp( m_flFogMaxDensity, 0.0f, 1.0f ); // Max fog density
				fogParams[3] = ooFogRange;
			}
			else
			{
				fogParams[0] = 1.0f - ( ooFogRange * fEnd );
				fogParams[1] = 0.0f;
				fogParams[2] = clamp( m_flFogMaxDensity, 0.0f, 1.0f ); // Max fog density
				fogParams[3] = ooFogRange;
			}
		}
		else
		{
			// Fixed-function-blended per-vertex fog requires some inverted params since a fog factor of 0 means fully fogged and 1 means no fog.
			// We could implement shader fog the same way, but would require an extra subtract in the vertex and/or pixel shader, which we want to avoid.
			if ( HardwareConfig()->GetDXSupportLevel() <= 90 )
			{
				//emulate no-fog by rigging the result to always be 0.
				fogParams[0] = 1.0f;
				fogParams[1] = -FLT_MAX;
				fogParams[2] = 1.0f; //Max fog density
				fogParams[3] = 0.0f; //0 out distance factor
			}
			else
			{
				//emulate no-fog by rigging the result to always be 0.
				fogParams[0] = 0.0f;
				fogParams[1] = -FLT_MAX;
				fogParams[2] = 0.0f; //Max fog density
				fogParams[3] = 0.0f; //0 out distance factor
			}
		}

		// cFogEndOverFogRange, cFogOne, unused, cOOFogRange
		SetVertexShaderConstantInternal( VERTEX_SHADER_FOG_PARAMS, fogParams, 1 );
		SetVertexShaderCameraPos();
	}

	// Compute stats info for a texture
	void ComputeStatsInfo( ShaderAPITextureHandle_t hTexture, bool bIsCubeMap, bool isVolumeTexture );

	// For procedural textures
	void AdvanceCurrentCopy( ShaderAPITextureHandle_t hTexture );

	// Deletes a D3D texture
	void DeleteD3DTexture( ShaderAPITextureHandle_t hTexture );

	// Unbinds a texture
	void UnbindTexture( ShaderAPITextureHandle_t hTexture );

	// Releases all D3D textures
	void ReleaseAllTextures();

	// Deletes all textures
	void DeleteAllTextures();

	// Gets the currently modified texture handle
	ShaderAPITextureHandle_t GetModifyTextureHandle() const;

	// Gets the bind id
	ShaderAPITextureHandle_t GetBoundTextureBindId( Sampler_t sampler ) const;

	// If mat_texture_limit is enabled, then this tells us if binding the specified texture would
	// take us over the limit.
	bool WouldBeOverTextureLimit( ShaderAPITextureHandle_t hTexture );

	// Sets the texture state
	void SetTextureState( Sampler_t sampler, TextureBindFlags_t nBindFlags, ShaderAPITextureHandle_t hTexture, bool force = false );

	FORCEINLINE void TouchTexture( Sampler_t sampler, IDirect3DBaseTexture *pD3DTexture );

	// Lookup standard texture handle
	ShaderAPITextureHandle_t GetStandardTextureHandle(StandardTextureId_t id);

	// Grab/release the internal render targets such as the back buffer and the save game thumbnail
	void AcquireInternalRenderTargets();
	void ReleaseInternalRenderTargets();

	// create/release linear->gamma table texture lookups. Only used by hardware supporting pixel shader 2b and up
	void AcquireLinearToGammaTableTextures();
	void ReleaseLinearToGammaTableTextures();

	// Gets the texture being modified
	IDirect3DBaseTexture* GetModifyTexture();
	void SetModifyTexture( IDirect3DBaseTexture *pTex );

	// returns true if we're using texture coordinates at a given stage
	bool IsUsingTextureCoordinates( int stage, int flags ) const;

	// Debugging spew
	void SpewBoardState();

	// Compute and save the world space camera position and direction
	void CacheWorldSpaceCamera();

	// Compute and save the projection atrix with polyoffset built in if we need it.
	void CachePolyOffsetProjectionMatrix();

	// Vertex shader helper functions
	int  FindVertexShader( VertexFormat_t fmt, const char* pFileName ) const;
	int  FindPixelShader( const char* pFileName ) const;

	// Returns copies of the front and back buffers
	IDirect3DSurface* GetFrontBufferImage( ImageFormat& format );
	IDirect3DSurface* GetBackBufferImage( Rect_t *pSrcRect, Rect_t *pDstRect, ImageFormat& format );
	IDirect3DSurface* GetBackBufferImageHDR( Rect_t *pSrcRect, Rect_t *pDstRect, ImageFormat& format );

	// Copy bits from a host-memory surface
	void CopyBitsFromHostSurface( IDirect3DSurface* pSurfaceBits,
		const Rect_t &dstRect, unsigned char *pData, ImageFormat srcFormat, ImageFormat dstFormat, int nDstStride );

	void SetTextureFilterMode( Sampler_t sampler, TextureFilterMode_t nMode );

	void ExecuteCommandBuffer( uint8 *pCmdBuffer );
#ifdef _PS3
	void ExecuteCommandBufferPPU(uint8 *pCmdBuffer );
#endif

	void ExecuteInstanceCommandBuffer( const unsigned char *pCmdBuf, int nInstanceIndex, bool bForceStateSet );
	void SetStandardTextureHandle( StandardTextureId_t nId, ShaderAPITextureHandle_t );
	void RecomputeAggregateLightingState( void );
	virtual void DrawInstances( int nInstanceCount, const MeshInstanceData_t *pInstance );

	// Methods related to queuing functions to be called per-(pMesh->Draw call) or per-pass
	void ClearAllCommitFuncs( CommitFuncType_t func );
	bool IsCommitFuncInUse( CommitFuncType_t func, int nFunc ) const;
	void MarkCommitFuncInUse( CommitFuncType_t func, int nFunc );
	void AddCommitFunc( CommitFuncType_t func, StateCommitFunc_t f );
	void CallCommitFuncs( CommitFuncType_t func, bool bForce = false );

	// Commits transforms and lighting
	void CommitStateChanges();

	// Commits transforms that have to be dealt with on a per pass basis (ie. projection matrix for polyoffset)
	void CommitPerPassStateChanges( StateSnapshot_t id );

	// Need to handle fog mode on a per-pass basis
	void CommitPerPassFogMode( bool bUsingVertexAndPixelShaders );

	void CommitPerPassXboxFixups();

	// Commits user clip planes
	void CommitUserClipPlanes( );

	// Gets the user clip transform (world->view)
	D3DXMATRIX & GetUserClipTransform( );

	// transform commit
	bool VertexShaderTransformChanged( int i );

	void UpdateVertexShaderMatrix( int iMatrix );
	void CommitVertexShaderTransforms();
	void CommitPerPassVertexShaderTransforms();

	// Recomputes the fast-clip plane matrices based on the current fast-clip plane
	void CommitFastClipPlane( );

	// Computes a matrix which includes the poly offset given an initial projection matrix
	void ComputePolyOffsetMatrix( const D3DXMATRIX& matProjection, D3DXMATRIX &matProjectionOffset );

	bool SetSkinningMatrices( const MeshInstanceData_t &instance );
	bool IsRenderingInstances() const;

	// lighting commit
	void CommitVertexShaderLighting( CompiledLightingState_t *pLightingState );
	void CommitPixelShaderLighting( int pshReg, CompiledLightingState_t *pLightingState );

	// Gets the surface associated with a texture (refcount of surface is increased)
	IDirect3DSurface* GetTextureSurface( ShaderAPITextureHandle_t textureHandle );
	IDirect3DSurface* GetDepthTextureSurface( ShaderAPITextureHandle_t textureHandle );

	//
	// Methods related to hardware config
	//
	void SetDefaultConfigValuesForDxLevel( int dxLevelFromCaps, ShaderDeviceInfo_t &info, unsigned int nFlagsUsed );

	// Determines hardware capabilities
	bool DetermineHardwareCaps( );

	// Alpha To Coverage entrypoints and states - much of this involves vendor-dependent paths and states...
	bool CheckVendorDependentAlphaToCoverage();
	void EnableAlphaToCoverage();
	void DisableAlphaToCoverage();

	// Vendor-dependent shadow mapping detection
	void CheckVendorDependentShadowMappingSupport( bool &bSupportsShadowDepthTextures, bool &bSupportsFetch4 );

	// Override caps based on a requested dx level
	void OverrideCaps( int nForcedDXLevel );

	// Reports support for a given MSAA mode
	bool SupportsMSAAMode( int nMSAAMode );

	// Reports support for a given CSAA mode
	bool SupportsCSAAMode( int nNumSamples, int nQualityLevel );

	// Gamma correction of fog color, or not...
	D3DCOLOR ComputeGammaCorrectedFogColor( unsigned char r, unsigned char g, unsigned char b, bool bSRGBWritesEnabled );

	bool RestorePersistedDisplay( bool bUseFrontBuffer );

	void ClearStdTextureHandles( void );

	// debug logging
	void PrintfVA( char *fmt, va_list vargs );
	void Printf( char *fmt, ... );
	float Knob( char *knobname, float *setvalue = NULL );

	void AddShaderComboInformation( const ShaderComboSemantics_t *pSemantics );

	virtual float GetLightMapScaleFactor() const;

	virtual ShaderAPITextureHandle_t FindTexture( const char *pDebugName );
	virtual void GetTextureDimensions( ShaderAPITextureHandle_t hTexture, int &nWidth, int &nHeight, int &nDepth );

#ifndef _GAMECONSOLE
	D3DDeviceWrapper	m_DeviceWrapper;
#endif

	// "normal" back buffer and depth buffer.  Need to keep this around so that we
	// know what to set the render target to when we are done rendering to a texture.
	CUtlVector<IDirect3DSurface*> m_pBackBufferSurfaces;
	IDirect3DSurface	*m_pBackBufferSurfaceSRGB;
	IDirect3DSurface	*m_pZBufferSurface;

	// Optimization for screenshots
	IDirect3DSurface	*m_pSmallBackBufferFP16TempSurface;

	ShaderAPITextureHandle_t m_hFullScreenTexture;

	ShaderAPITextureHandle_t m_hLinearToGammaTableTexture;
	ShaderAPITextureHandle_t m_hLinearToGammaTableIdentityTexture;

	//
	// State needed at the time of rendering (after snapshots have been applied)
	//

	// Interface for the D3DXMatrixStack
	ID3DXMatrixStack*	m_pMatrixStack[NUM_MATRIX_MODES];
	matrix3x4_t			m_boneMatrix[NUM_MODEL_TRANSFORMS];
	int					m_maxBoneLoaded;

	// Current matrix mode
	D3DTRANSFORMSTATETYPE m_MatrixMode;
	int m_CurrStack;

	// The current camera position in world space.
	Vector m_WorldSpaceCameraPosition;
	Vector m_WorldSpaceCameraDirection;

	// The current projection matrix with polyoffset baked into it.
	D3DXMATRIX		m_CachedPolyOffsetProjectionMatrix;
	D3DXMATRIX		m_CachedFastClipProjectionMatrix;
	D3DXMATRIX		m_CachedFastClipPolyOffsetProjectionMatrix;

	// The texture stage state that changes frequently
	DynamicState_t	m_DynamicState;

	// The *desired* dynamic state. Most dynamic state is committed into actual hardware state
	// at either per-pass or per-material time.	This can also be used to force the hardware
	// to match the desired state after returning from alt-tab.
	DynamicState_t	m_DesiredState;

	// A list of state commit functions to run as per-draw call commit time
	unsigned char m_pCommitFlags[COMMIT_FUNC_TYPE_COUNT][ COMMIT_FUNC_BYTE_COUNT ];
	CUtlVector< StateCommitFunc_t >	m_CommitFuncs[COMMIT_FUNC_TYPE_COUNT];

	// Render data
	CMeshBase *m_pRenderMesh;
	int m_nRenderInstanceCount;
	const MeshInstanceData_t *m_pRenderInstances;
	CompiledLightingState_t *m_pRenderCompiledState;
	InstanceInfo_t *m_pRenderInstanceInfo;
	ShaderStencilState_t m_RenderInitialStencilState;
	bool m_bRenderHasSetStencil;

	int m_nDynamicVBSize;
	IMaterialInternal *m_pMaterial;

	// Shadow depth bias states
	float m_fShadowSlopeScaleDepthBias;
	float m_fShadowDepthBias;

	bool		m_bReadPixelsEnabled : 1;
	bool		m_bFlipCulling : 1;
	bool		m_bSinglePassFlashlightMode : 1;
	bool		m_UsingTextureRenderTarget : 1;

	int			m_ViewportMaxWidth;
	int			m_ViewportMaxHeight;

	ShaderAPITextureHandle_t m_hCachedRenderTarget;
	bool		m_bUsingSRGBRenderTarget;

	// The current frame
	int m_CurrentFrame;

	// The texture we're currently modifying
	// Using thread local variables so that ModifyTexture(), TexLock()/TexUnlock(), ... can concurently be used
	// from different thread (eg scaleform and lightmap)
	static CTHREADLOCALINTEGER( ShaderAPITextureHandle_t ) m_ModifyTextureHandle;
	static CTHREADLOCALINT m_ModifyTextureLockedLevel;
	static CTHREADLOCALINT m_ModifyTextureLockedFace;

	// Stores all textures
	CUtlFixedLinkedList< Texture_t >	m_Textures;
#ifdef _PS3
	CUtlVector< ShaderAPITextureHandle_t > m_ArtificialTextureHandles;
	struct DepthBufferCacheEntry_t
	{
		CPs3gcmTexture *m_pReal;
		CPs3gcmTexture *m_pCache;
	};
	CUtlVector< DepthBufferCacheEntry_t > m_arrPs3DepthBufferCache;
#endif

	float			m_VertexShaderFogParams[2];
	float			m_flFogMaxDensity;

	// Shadow state transition table
	CTransitionTable m_TransitionTable;
	StateSnapshot_t m_nCurrentSnapshot;

	// Depth test override...
	bool	m_bOverrideMaterialIgnoreZ;
	bool 	m_bIgnoreZValue;

	// Are we in the middle of resetting the render state?
	bool	m_bResettingRenderState;

	// Can we buffer 2 frames ahead?
	bool	m_bBuffer2FramesAhead;

	// Selection name stack
	CUtlStack< int >	m_SelectionNames;
	bool				m_InSelectionMode;
	unsigned int*		m_pSelectionBufferEnd;
	unsigned int*		m_pSelectionBuffer;
	unsigned int*		m_pCurrSelectionRecord;
	float				m_SelectionMinZ;
	float				m_SelectionMaxZ;
	int					m_NumHits;

	// fog
	unsigned char m_SceneFogColor[3];
	MaterialFogMode_t m_SceneFogMode;

	// Tone Mapping state ( w is gamma scale )
	Vector4D m_ToneMappingScale;

	Deformation_t m_DeformationStack[DEFORMATION_STACK_DEPTH];

	Deformation_t *m_pDeformationStackPtr;

	void WriteShaderConstantsToGPU();

	// rendering parameter storage
	int IntRenderingParameters[MAX_INT_RENDER_PARMS];
	ITexture *TextureRenderingParameters[MAX_TEXTURE_RENDER_PARMS];
	float FloatRenderingParameters[MAX_FLOAT_RENDER_PARMS];
	Vector VectorRenderingParameters[MAX_VECTOR_RENDER_PARMS];

	ShaderAPITextureHandle_t m_StdTextureHandles[TEXTURE_MAX_STD_TEXTURES];

	// PIX instrumentation utilities...enable these with PIX_INSTRUMENTATION
	void StartPIXInstrumentation();
	void EndPIXInstrumentation();
	void SetPIXMarker( unsigned long color, const char *szName );
	void IncrementPIXError();
	bool PIXError();
	int m_nPIXErrorCount;
	int m_nPixFrame;
	bool m_bPixCapturing;

	void ComputeVertexDescription( unsigned char* pBuffer, VertexFormat_t vertexFormat, MeshDesc_t& desc ) const
	{
		return MeshMgr()->ComputeVertexDescription( pBuffer, vertexFormat, desc );
	}

	int VertexFormatSize( VertexFormat_t vertexFormat ) const
	{
		return MeshMgr()->VertexFormatSize( vertexFormat );
	}

	// Bracket custom shadow rendering code that doesn't use transition tables or regular cshaders
	bool m_bToolsMode;
	bool m_bVtxLitMesh;
	bool m_bUnlitMesh;
	bool m_bLmapMesh;
	bool m_bGeneratingCSMs;
	BOOL m_bCSMsValidThisFrame;
	void BeginGeneratingCSMs();
	void EndGeneratingCSMs();

	// Per-cascade settings
	void PerpareForCascadeDraw( int cascade, float fShadowSlopeScaleDepthBias, float fShadowDepthBias );

	void SetShadowDepthBiasFactors( float fShadowSlopeScaleDepthBias, float fShadowDepthBias );

	void EnableBuffer2FramesAhead( bool bEnable );

	void GetActualProjectionMatrix( float *pMatrix );

	void SetDepthFeatheringShaderConstants( int iConstant, float fDepthBlendScale );

	void SetDisallowAccess( bool b )
	{
		g_bShaderAccessDisallowed = b;
	}

	void EnableShaderShaderMutex( bool b )
	{
		Assert( g_ShaderMutex.GetOwnerId() == 0 );
		g_bUseShaderMutex = b;
	}

	void ShaderLock()
	{
		g_ShaderMutex.Lock();
	}

	void ShaderUnlock()
	{
		g_ShaderMutex.Unlock();
	}

	//The idea behind a delayed constant is this.
	// Some shaders set constants based on rendering states, and some rendering states aren't updated until after a shader's already called Draw().
	//  So, for some functions that are state based, we save the constant we set and if the state changes between when it's set in the shader setup code
	//   and when the shader is drawn, we update that constant.
	struct DelayedConstants_t
	{
		int iPixelShaderFogParams;

		void Invalidate( void )
		{
			iPixelShaderFogParams = -1;
		}
		DelayedConstants_t( void ) { this->Invalidate(); }
	};
	DelayedConstants_t m_DelayedShaderConstants;

	bool SetRenderTargetInternalXbox( ShaderAPITextureHandle_t hTexture, bool bForce = false );

	FogMethod_t ComputeFogMethod( ShaderFogMode_t shaderFogMode, MaterialFogMode_t sceneFogMode, bool bVertexFog );

#if defined( _X360 )
	CUtlStack<int> m_VertexShaderGPRAllocationStack;
#endif

	int	m_MaxVectorVertexShaderConstant;
	int	m_MaxBooleanVertexShaderConstant;
	int	m_MaxIntegerVertexShaderConstant;
	int	m_MaxVectorPixelShaderConstant;
	int	m_MaxBooleanPixelShaderConstant;
	int	m_MaxIntegerPixelShaderConstant;

	bool m_bGPUOwned;
	bool m_bResetRenderStateNeeded;
#if defined( _GAMECONSOLE )
	bool m_bInZPass;
	unsigned int m_nZPassCounter;
	StateSnapshot_t m_zPassSnapshot;
#endif

	float32 m_flCurrGameTime;

#if defined( _X360 )
	struct XboxFontMap_t
	{
		XboxFontMap_t()
		{
			m_pPhysicalMemory = NULL;
			m_nMemorySize = 0;
			m_nFontFileSize = 0;
		}

		void	*m_pPhysicalMemory;
		int		m_nMemorySize;
		int		m_nFontFileSize;
	};

	CUtlDict< XboxFontMap_t > m_XboxFontMemoryDict;
#endif

#if defined( _WIN32 )
	IDirect3DSurface *m_pNVAPI_registeredDepthStencilSurface;
	IDirect3DTexture *m_pNVAPI_registeredDepthTexture;
#endif
};


//-----------------------------------------------------------------------------
// Class Factory
//-----------------------------------------------------------------------------
static CShaderAPIDx8 g_ShaderAPIDX8;
IShaderAPIDX8 *g_pShaderAPIDX8 = &g_ShaderAPIDX8;
CShaderDeviceDx8* g_pShaderDeviceDx8 = &g_ShaderAPIDX8;

// FIXME: Remove IShaderAPI + IShaderDevice; they change after SetMode
EXPOSE_SINGLE_INTERFACE_GLOBALVAR( CShaderAPIDx8, IShaderAPI,
				SHADERAPI_INTERFACE_VERSION, g_ShaderAPIDX8 )

EXPOSE_SINGLE_INTERFACE_GLOBALVAR( CShaderAPIDx8, IShaderDevice,
				SHADER_DEVICE_INTERFACE_VERSION, g_ShaderAPIDX8 )

EXPOSE_SINGLE_INTERFACE_GLOBALVAR( CShaderAPIDx8, IDebugTextureInfo,
				DEBUG_TEXTURE_INFO_VERSION, g_ShaderAPIDX8 )

//-----------------------------------------------------------------------------
// CShaderAPIDx8 static members
//-----------------------------------------------------------------------------
CTHREADLOCALINTEGER( ShaderAPITextureHandle_t ) CShaderAPIDx8::m_ModifyTextureHandle(INVALID_SHADERAPI_TEXTURE_HANDLE);
CTHREADLOCALINT CShaderAPIDx8::m_ModifyTextureLockedLevel(-1);
CTHREADLOCALINT CShaderAPIDx8::m_ModifyTextureLockedFace;

//-----------------------------------------------------------------------------
// Accessors for major interfaces
//-----------------------------------------------------------------------------
#if !defined( _GAMECONSOLE )
MTL::Device *MtlDevice()
{
	return g_ShaderAPIDX8.Dx9Device();
}
#endif